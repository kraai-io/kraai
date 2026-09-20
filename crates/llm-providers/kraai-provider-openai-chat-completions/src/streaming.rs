use std::collections::VecDeque;

use color_eyre::eyre::{Result, eyre};
use futures::{StreamExt, stream, stream::BoxStream};
use kraai_provider_core::{ProviderStreamEvent, SseEvent};
use kraai_types::AssistantPhase;

use crate::usage::normalize_usage;
use crate::wire::ChatCompletionChunk;

pub(super) fn adapt_chat_completion_stream(
    source: BoxStream<'static, Result<SseEvent>>,
    reported_costs: bool,
) -> BoxStream<'static, Result<ProviderStreamEvent>> {
    stream::unfold(
        (
            source,
            VecDeque::<Result<ProviderStreamEvent>>::new(),
            false,
        ),
        move |(mut source, mut pending, finished)| async move {
            if finished {
                return None;
            }

            loop {
                if let Some(event) = pending.pop_front() {
                    let failed = event.is_err();
                    return Some((event, (source, pending, failed)));
                }

                match source.next().await {
                    Some(Ok(SseEvent::Data(payload))) => {
                        match serde_json::from_str::<ChatCompletionChunk>(&payload) {
                            Ok(ChatCompletionChunk {
                                error: Some(error), ..
                            }) => {
                                return Some((
                                    Err(eyre!("Chat completions stream failed: {}", error.message)),
                                    (source, pending, true),
                                ));
                            }
                            Ok(chunk) => queue_chunk_events(chunk, reported_costs, &mut pending),
                            Err(error) => pending.push_back(Err(eyre!(error))),
                        }
                    }
                    Some(Ok(SseEvent::Done)) => return None,
                    Some(Err(error)) => {
                        return Some((Err(error), (source, pending, true)));
                    }
                    None => {
                        return Some((
                            Err(eyre!(
                                "Chat completions stream ended before the [DONE] marker"
                            )),
                            (source, pending, true),
                        ));
                    }
                }
            }
        },
    )
    .boxed()
}

fn queue_chunk_events(
    chunk: ChatCompletionChunk,
    reported_costs: bool,
    events: &mut VecDeque<Result<ProviderStreamEvent>>,
) {
    let incomplete_reason = chunk
        .choices
        .iter()
        .find_map(|choice| {
            choice
                .finish_reason
                .as_deref()
                .filter(|reason| !matches!(*reason, "stop" | "tool_calls" | "function_call"))
        })
        .map(str::to_owned);

    if let Some(delta) = chunk
        .choices
        .into_iter()
        .find_map(|choice| choice.delta.content)
    {
        events.push_back(Ok(ProviderStreamEvent::TextDelta {
            item_id: String::from("chat-completions-message"),
            phase: AssistantPhase::FinalAnswer,
            delta,
        }));
    }
    if let Some(usage) = chunk
        .usage
        .and_then(|usage| normalize_usage(usage, reported_costs))
    {
        events.push_back(Ok(ProviderStreamEvent::Usage(usage)));
    }
    if let Some(reason) = incomplete_reason {
        events.push_back(Err(eyre!(
            "Chat completions response did not complete successfully: {reason}"
        )));
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "stream tests use direct assertions for event fixtures"
)]
mod tests {
    use super::*;
    use futures::TryStreamExt;
    use std::time::Duration;

    #[test]
    fn normalize_usage_splits_cache_and_reasoning_tokens() {
        let usage = normalize_usage(
            crate::wire::ChatCompletionUsage {
                cost: None,
                cost_details: None,
                prompt_tokens: 120,
                completion_tokens: 45,
                total_tokens: Some(165),
                prompt_tokens_details: Some(crate::wire::PromptTokenDetails {
                    cached_tokens: Some(20),
                    cache_write_tokens: None,
                }),
                completion_tokens_details: Some(crate::wire::CompletionTokenDetails {
                    reasoning_tokens: Some(5),
                }),
            },
            false,
        )
        .expect("usage should normalize");

        assert_eq!(usage.total_tokens, 165);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 40);
        assert_eq!(usage.reasoning_tokens, 5);
        assert_eq!(usage.cache_read_tokens, 20);
    }

    #[test]
    fn mixed_stream_chunk_preserves_text_before_usage() {
        let chunk = serde_json::from_str::<ChatCompletionChunk>(
            r#"{
                "choices":[{"delta":{"content":"hello"}}],
                "usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}
            }"#,
        )
        .unwrap();

        let mut events = VecDeque::new();
        queue_chunk_events(chunk, false, &mut events);
        let events = events.into_iter().collect::<Result<Vec<_>>>().unwrap();

        assert_eq!(
            events,
            vec![
                ProviderStreamEvent::TextDelta {
                    item_id: String::from("chat-completions-message"),
                    phase: AssistantPhase::FinalAnswer,
                    delta: String::from("hello"),
                },
                ProviderStreamEvent::Usage(kraai_types::TokenUsage {
                    total_tokens: 3,
                    input_tokens: 2,
                    output_tokens: 1,
                    reasoning_tokens: 0,
                    cache_read_tokens: 0,
                    ..Default::default()
                }),
            ]
        );
    }

    #[tokio::test]
    async fn stream_rejects_eof_before_done_marker() {
        let source = stream::iter(vec![Ok(SseEvent::Data(String::from(
            r#"{"choices":[{"delta":{"content":"partial"}}]}"#,
        )))])
        .boxed();
        let events = adapt_chat_completion_stream(source, false)
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            events.first(),
            Some(Ok(ProviderStreamEvent::TextDelta { delta, .. })) if delta == "partial"
        ));
        assert!(events.get(1).is_some_and(Result::is_err));
    }

    #[tokio::test]
    async fn stream_stops_after_json_error() {
        let source = stream::iter([
            Ok(SseEvent::Data(String::from("{}"))),
            Ok(SseEvent::Data(String::from("invalid JSON"))),
            Ok(SseEvent::Data(String::from(
                r#"{"choices":[{"delta":{"content":"first"}}],"usage":{"prompt_tokens":2,"completion_tokens":1}}"#,
            ))),
            Ok(SseEvent::Data(String::from(
                r#"{"choices":[{"delta":{"content":"second"}}]}"#,
            ))),
            Ok(SseEvent::Done),
        ])
        .boxed();
        let events = adapt_chat_completion_stream(source, false)
            .collect::<Vec<_>>()
            .await;

        assert_eq!(events.len(), 1);
        assert!(events.first().is_some_and(Result::is_err));
    }

    #[tokio::test]
    async fn stream_surfaces_provider_errors_after_partial_output() {
        for failure in [
            r#"{"error":{"code":429,"message":"Rate limit exceeded"},"choices":[{"index":0,"delta":{"content":""},"finish_reason":"error"}]}"#,
            r#"{"error":{"code":"server_error","message":"Rate limit exceeded"}}"#,
        ] {
            let source = stream::iter([
                Ok(SseEvent::Data(String::from(
                    r#"{"choices":[{"delta":{"content":"partial"}}]}"#,
                ))),
                Ok(SseEvent::Data(failure.to_string())),
                Ok(SseEvent::Done),
            ])
            .boxed();
            let events = adapt_chat_completion_stream(source, false)
                .collect::<Vec<_>>()
                .await;

            assert!(matches!(
                events.first(),
                Some(Ok(ProviderStreamEvent::TextDelta { delta, .. })) if delta == "partial"
            ));
            assert!(
                events.get(1).is_some_and(|event| {
                    event
                        .as_ref()
                        .is_err_and(|error| error.to_string().contains("Rate limit exceeded"))
                }),
                "provider error was swallowed: {events:?}"
            );
            assert_eq!(events.len(), 2);
        }
    }

    #[tokio::test]
    async fn provider_error_terminates_without_waiting_for_done() {
        let source = stream::iter([Ok(SseEvent::Data(String::from(
            r#"{"error":{"message":"upstream stopped"}}"#,
        )))])
        .chain(stream::pending())
        .boxed();
        let mut events = adapt_chat_completion_stream(source, false);
        let error = tokio::time::timeout(Duration::from_secs(1), events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("upstream stopped"));
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn null_error_preserves_text_usage_and_normal_finish_reasons() {
        for finish_reason in ["stop", "tool_calls", "function_call"] {
            let source = stream::iter([
                Ok(SseEvent::Data(format!(
                    r#"{{"error":null,"choices":[{{"delta":{{"content":"reply"}},"finish_reason":"{finish_reason}"}}],"usage":{{"prompt_tokens":2,"completion_tokens":1}}}}"#,
                ))),
                Ok(SseEvent::Done),
            ])
            .boxed();
            let events = adapt_chat_completion_stream(source, false)
                .try_collect::<Vec<_>>()
                .await
                .unwrap();
            assert!(matches!(
                events.first(),
                Some(ProviderStreamEvent::TextDelta { delta, .. }) if delta == "reply"
            ));
            assert!(matches!(events.get(1), Some(ProviderStreamEvent::Usage(_))));
            assert_eq!(events.len(), 2);
        }
    }

    #[tokio::test]
    async fn stream_stops_at_done_without_waiting_for_eof() {
        let source = stream::iter(vec![Ok(SseEvent::Done)])
            .chain(stream::pending())
            .boxed();

        let event = tokio::time::timeout(
            Duration::from_secs(1),
            adapt_chat_completion_stream(source, false).next(),
        )
        .await
        .unwrap();

        assert!(event.is_none());
    }

    #[tokio::test]
    async fn stream_rejects_incomplete_finish_reasons_and_preserves_usage() {
        for reason in ["length", "content_filter", "unknown_failure"] {
            let source = stream::iter(vec![
                Ok(SseEvent::Data(String::from(
                    r#"{"choices":[{"delta":{"content":"Partial summary"}}]}"#,
                ))),
                Ok(SseEvent::Data(format!(
                    r#"{{"choices":[{{"delta":{{}},"finish_reason":"{reason}"}}],"usage":{{"prompt_tokens":2,"completion_tokens":1}}}}"#,
                ))),
                Ok(SseEvent::Done),
            ]).boxed();
            let events = adapt_chat_completion_stream(source, false)
                .collect::<Vec<_>>()
                .await;
            assert_eq!(events.len(), 3);
            assert!(matches!(
                events.first(),
                Some(Ok(ProviderStreamEvent::TextDelta { .. }))
            ));
            assert!(matches!(
                events.get(1),
                Some(Ok(ProviderStreamEvent::Usage(_)))
            ));
            assert!(events.get(2).is_some_and(|event| {
                event
                    .as_ref()
                    .is_err_and(|error| error.to_string().contains(reason))
            }));
        }
    }

    #[tokio::test]
    async fn stream_accepts_successful_text_and_tool_finish_reasons() {
        for reason in ["stop", "tool_calls", "function_call"] {
            let source = stream::iter(vec![
                Ok(SseEvent::Data(format!(
                    r#"{{"choices":[{{"delta":{{"content":"complete"}},"finish_reason":"{reason}"}}]}}"#,
                ))),
                Ok(SseEvent::Done),
            ]).boxed();
            let events = adapt_chat_completion_stream(source, false)
                .collect::<Vec<_>>()
                .await;
            assert_eq!(events.len(), 1);
            assert!(events.iter().all(Result::is_ok));
        }
    }
}
