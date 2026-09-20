use std::collections::HashMap;

use color_eyre::eyre::{Result, eyre};
use futures::{StreamExt, stream, stream::BoxStream};
use kraai_provider_core::{ProviderStreamEvent, SseEvent};
use kraai_types::{AssistantPhase, ToolCallId};

use crate::wire::{ResponsesError, ResponsesStreamEvent, ResponsesUsage};

pub(crate) fn adapt_responses_stream(
    source: BoxStream<'static, Result<SseEvent>>,
) -> BoxStream<'static, Result<ProviderStreamEvent>> {
    stream::unfold(
        (source, false, HashMap::<String, AssistantPhase>::new()),
        |(mut source, finished, mut phases)| async move {
            if finished {
                return None;
            }

            loop {
                let event = match source.next().await {
                    Some(Ok(SseEvent::Data(payload))) => {
                        match serde_json::from_str::<ResponsesStreamEvent>(&payload) {
                            Ok(event) if event.kind == "error" => {
                                let error = match serde_json::from_str::<ResponsesError>(&payload) {
                                    Ok(error) => eyre!(
                                        "OpenAI response stream failed: {}",
                                        format_response_error(error)
                                    ),
                                    Err(error) => eyre!(error),
                                };
                                return Some((Err(error), (source, true, phases)));
                            }
                            Ok(event) => event,
                            Err(error) => {
                                return Some((Err(eyre!(error)), (source, true, phases)));
                            }
                        }
                    }
                    Some(Ok(SseEvent::Done)) | None => {
                        return Some((
                            Err(eyre!(
                                "OpenAI response stream ended before response.completed"
                            )),
                            (source, true, phases),
                        ));
                    }
                    Some(Err(error)) => return Some((Err(error), (source, true, phases))),
                };

                match event.kind.as_str() {
                    "response.output_item.added" => {
                        if let Some(item) = event.item
                            && item.kind == "message"
                            && let Some(item_id) = item.id
                        {
                            phases.insert(item_id, parse_phase(item.phase.as_deref()));
                        }
                    }
                    "response.output_text.delta" => {
                        if let Some(delta) = event.delta {
                            let Some(item_id) = event.item_id else {
                                return Some((
                                    Err(eyre!("OpenAI output text delta omitted item_id")),
                                    (source, true, phases),
                                ));
                            };
                            let phase = phases
                                .get(&item_id)
                                .copied()
                                .unwrap_or(AssistantPhase::FinalAnswer);
                            return Some((
                                Ok(ProviderStreamEvent::TextDelta {
                                    item_id,
                                    phase,
                                    delta,
                                }),
                                (source, false, phases),
                            ));
                        }
                    }
                    "response.output_item.done" => {
                        if let Some(item) = event.item
                            && item.kind == "custom_tool_call"
                        {
                            let Some(call_id) = item.call_id else {
                                return Some((
                                    Err(eyre!("OpenAI custom tool call omitted call_id")),
                                    (source, true, phases),
                                ));
                            };
                            let Some(name) = item.name else {
                                return Some((
                                    Err(eyre!("OpenAI custom tool call omitted name")),
                                    (source, true, phases),
                                ));
                            };
                            let Some(input) = item.input else {
                                return Some((
                                    Err(eyre!("OpenAI custom tool call omitted input")),
                                    (source, true, phases),
                                ));
                            };
                            let call_id = match ToolCallId::try_new(call_id) {
                                Ok(call_id) => call_id,
                                Err(error) => {
                                    return Some((Err(eyre!(error)), (source, true, phases)));
                                }
                            };
                            return Some((
                                Ok(ProviderStreamEvent::ScriptCall {
                                    call_id,
                                    name,
                                    input,
                                }),
                                (source, false, phases),
                            ));
                        }
                    }
                    "response.completed" => {
                        let usage = event
                            .response
                            .and_then(|response| response.usage)
                            .and_then(normalize_usage);
                        return Some((
                            usage.map_or_else(
                                || Err(eyre!("OpenAI response.completed event omitted usage")),
                                |usage| Ok(ProviderStreamEvent::Usage(usage)),
                            ),
                            (source, true, phases),
                        ));
                    }
                    "response.failed" | "response.incomplete" => {
                        let detail = event
                            .response
                            .map(format_response_failure)
                            .unwrap_or_else(|| String::from("no failure details were provided"));
                        return Some((
                            Err(eyre!("OpenAI response stream failed: {detail}")),
                            (source, true, phases),
                        ));
                    }
                    _ => {}
                }
            }
        },
    )
    .boxed()
}

fn parse_phase(phase: Option<&str>) -> AssistantPhase {
    match phase {
        Some("commentary") => AssistantPhase::Commentary,
        _ => AssistantPhase::FinalAnswer,
    }
}

fn format_response_failure(response: crate::wire::ResponsesCompletedResponse) -> String {
    if let Some(error) = response.error {
        return format_response_error(error);
    }
    response.incomplete_details.map_or_else(
        || String::from("no failure details were provided"),
        |details| details.to_string(),
    )
}

fn format_response_error(error: ResponsesError) -> String {
    match error.code {
        Some(code) => format!("{code}: {}", error.message),
        None => error.message,
    }
}

fn normalize_usage(usage: ResponsesUsage) -> Option<kraai_types::TokenUsage> {
    let cache_read_tokens = usage
        .input_tokens_details
        .and_then(|details| details.cached_tokens)
        .unwrap_or_default();
    let reasoning_tokens = usage
        .output_tokens_details
        .and_then(|details| details.reasoning_tokens)
        .unwrap_or_default();
    let input_tokens = usage.input_tokens.saturating_sub(cache_read_tokens);
    let output_tokens = usage.output_tokens.saturating_sub(reasoning_tokens);
    let total_tokens = usage.input_tokens.saturating_add(usage.output_tokens);

    if total_tokens == 0
        && input_tokens == 0
        && output_tokens == 0
        && reasoning_tokens == 0
        && cache_read_tokens == 0
    {
        return None;
    }

    Some(kraai_types::TokenUsage {
        total_tokens,
        input_tokens,
        output_tokens,
        reasoning_tokens,
        cache_read_tokens,
        ..Default::default()
    })
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests use direct assertions for stream fixtures"
)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn normalize_usage_splits_cache_and_reasoning_tokens() {
        let usage = normalize_usage(ResponsesUsage {
            input_tokens: 120,
            output_tokens: 45,
            input_tokens_details: Some(crate::wire::ResponsesInputTokenDetails {
                cached_tokens: Some(20),
            }),
            output_tokens_details: Some(crate::wire::ResponsesOutputTokenDetails {
                reasoning_tokens: Some(5),
            }),
        })
        .expect("usage should normalize");

        assert_eq!(usage.total_tokens, 165);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 40);
        assert_eq!(usage.reasoning_tokens, 5);
        assert_eq!(usage.cache_read_tokens, 20);
    }

    #[tokio::test]
    async fn responses_stream_rejects_eof_before_completed() {
        let source = stream::iter(vec![
            Ok(SseEvent::Data(String::from(
                r#"{"type":"response.output_item.added","item":{"type":"message","id":"msg-1","phase":"commentary"}}"#,
            ))),
            Ok(SseEvent::Data(String::from(
                r#"{"type":"response.output_text.delta","item_id":"msg-1","delta":"partial"}"#,
            ))),
        ])
        .boxed();
        let events = adapt_responses_stream(source).collect::<Vec<_>>().await;

        assert!(matches!(
            events.first(),
            Some(Ok(ProviderStreamEvent::TextDelta { phase: AssistantPhase::Commentary, delta, .. })) if delta == "partial"
        ));
        assert!(events.get(1).is_some_and(Result::is_err));
    }

    #[tokio::test]
    async fn responses_stream_rejects_completed_event_without_usage() {
        let source = stream::iter(vec![Ok(SseEvent::Data(String::from(
            r#"{"type":"response.completed","response":{}}"#,
        )))])
        .chain(stream::pending())
        .boxed();

        let event = tokio::time::timeout(
            Duration::from_secs(1),
            adapt_responses_stream(source).next(),
        )
        .await
        .unwrap();

        assert!(event.is_some_and(|result| result.is_err()));
    }

    #[tokio::test]
    async fn responses_stream_preserves_error_event_details_after_partial_text() {
        for code in [r#""server_error""#, "null"] {
            let source = stream::iter(vec![
                Ok(SseEvent::Data(String::from(
                    r#"{"type":"response.output_text.delta","item_id":"msg-1","delta":"partial"}"#,
                ))),
                Ok(SseEvent::Data(format!(
                    r#"{{"type":"error","code":{code},"message":"upstream failed","param":null,"sequence_number":2}}"#,
                ))),
            ])
            .boxed();
            let events = adapt_responses_stream(source).collect::<Vec<_>>().await;

            assert!(matches!(
                events.first(),
                Some(Ok(ProviderStreamEvent::TextDelta { delta, .. })) if delta == "partial"
            ));
            let error = events.get(1).unwrap().as_ref().unwrap_err().to_string();
            let detail = if code == "null" {
                "upstream failed"
            } else {
                "server_error: upstream failed"
            };
            assert_eq!(error, format!("OpenAI response stream failed: {detail}"));
            assert_eq!(events.len(), 2);
        }
    }

    #[tokio::test]
    async fn responses_error_event_terminates_without_waiting_for_transport() {
        let source = stream::iter(vec![Ok(SseEvent::Data(String::from(
            r#"{"type":"error","message":"request failed"}"#,
        )))])
        .chain(stream::pending())
        .boxed();
        let mut events = adapt_responses_stream(source);
        let error = tokio::time::timeout(Duration::from_secs(1), events.next())
            .await
            .expect("error event must not wait for transport EOF")
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "OpenAI response stream failed: request failed"
        );
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn responses_stream_preserves_native_custom_call_identity_and_input() {
        let source = stream::iter(vec![
            Ok(SseEvent::Data(String::from(
                r##"{"type":"response.output_item.done","item":{"type":"custom_tool_call","id":"item-1","call_id":"call-123","name":"kraai_nushell","input":"# timeout=30sec\nls"}}"##,
            ))),
            Ok(SseEvent::Data(String::from(
                r#"{"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":5}}}"#,
            ))),
        ])
        .boxed();

        let events = adapt_responses_stream(source).collect::<Vec<_>>().await;

        assert!(matches!(
            events.first(),
            Some(Ok(ProviderStreamEvent::ScriptCall { call_id, name, input }))
                if call_id.as_str() == "call-123"
                    && name == "kraai_nushell"
                    && input == "# timeout=30sec\nls"
        ));
        assert!(matches!(
            events.get(1),
            Some(Ok(ProviderStreamEvent::Usage(_)))
        ));
    }
}
