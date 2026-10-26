use std::collections::{HashMap, VecDeque};

use color_eyre::eyre::{Result, ensure, eyre};
use futures::stream::BoxStream;
use kraai_provider_core::{
    ProviderError, ProviderStreamEvent, SseEvent, StopReason, StreamStatus, adapt_provider_stream,
};
use kraai_types::{AssistantPhase, ScriptCall, ToolCallId};

use crate::wire::{ResponsesError, ResponsesStreamEvent, ResponsesUsage};

pub(crate) fn adapt_responses_stream(
    source: BoxStream<'static, Result<SseEvent>>,
) -> BoxStream<'static, Result<ProviderStreamEvent>> {
    let mut completion = Completion::default();
    adapt_provider_stream(
        source,
        "OpenAI response stream ended before response.completed",
        move |event, pending| {
            let SseEvent::Data(payload) = event else {
                return Err(ProviderError::StreamInterrupted(
                    "OpenAI response stream ended before response.completed".into(),
                )
                .into());
            };
            let event: ResponsesStreamEvent = serde_json::from_str(&payload)?;
            if event.kind == "error" {
                let error: ResponsesError = serde_json::from_str(&payload)?;
                let code = error.code.clone();
                return Err(ProviderError::from_stream_error(
                    code.as_deref(),
                    format!(
                        "OpenAI response stream failed: {}",
                        format_response_error(error)
                    ),
                )
                .into());
            }
            completion.ingest(event, pending)
        },
    )
}

#[derive(Default)]
struct Completion {
    phases: HashMap<String, AssistantPhase>,
    called_tool: bool,
}

impl Completion {
    fn ingest(
        &mut self,
        event: ResponsesStreamEvent,
        pending: &mut VecDeque<ProviderStreamEvent>,
    ) -> Result<StreamStatus> {
        match event.kind.as_str() {
            "response.output_item.added" => {
                if let Some(item) = event.item
                    && item.kind == "message"
                    && let Some(item_id) = item.id
                {
                    self.phases
                        .insert(item_id, parse_phase(item.phase.as_deref()));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(delta) = event.delta.filter(|delta| !delta.is_empty()) {
                    let item_id = event
                        .item_id
                        .ok_or_else(|| eyre!("OpenAI reasoning delta omitted item_id"))?;
                    pending.push_back(ProviderStreamEvent::ReasoningDelta { item_id, delta });
                }
            }
            "response.output_text.delta" => {
                if let Some(delta) = event.delta {
                    let item_id = event
                        .item_id
                        .ok_or_else(|| eyre!("OpenAI output text delta omitted item_id"))?;
                    let phase = self
                        .phases
                        .get(&item_id)
                        .copied()
                        .unwrap_or(AssistantPhase::FinalAnswer);
                    pending.push_back(ProviderStreamEvent::TextDelta {
                        item_id,
                        phase,
                        delta,
                    });
                }
            }
            "response.output_item.done" => {
                if let Some(item) = event.item {
                    match item.kind.as_str() {
                        "compaction" => {
                            ensure!(
                                item.extra
                                    .get("encrypted_content")
                                    .and_then(serde_json::Value::as_str)
                                    .is_some_and(|content| !content.is_empty()),
                                "OpenAI compaction item omitted encrypted content"
                            );
                            pending.push_back(ProviderStreamEvent::Compaction {
                                payload: serde_json::to_value(item)?,
                            });
                        }
                        "reasoning"
                            if item
                                .extra
                                .get("encrypted_content")
                                .is_some_and(|value| !value.is_null()) =>
                        {
                            ensure!(
                                item.id.as_deref().is_some_and(|id| !id.is_empty())
                                    && item
                                        .extra
                                        .get("encrypted_content")
                                        .and_then(serde_json::Value::as_str)
                                        .is_some_and(|content| !content.is_empty()),
                                "OpenAI encrypted reasoning item is missing a valid id or encrypted content"
                            );
                            pending.push_back(ProviderStreamEvent::Reasoning {
                                payload: serde_json::to_value(item)?,
                            });
                        }
                        "custom_tool_call" => {
                            let call_id = item
                                .call_id
                                .ok_or_else(|| eyre!("OpenAI custom tool call omitted call_id"))?;
                            let name = item
                                .name
                                .ok_or_else(|| eyre!("OpenAI custom tool call omitted name"))?;
                            let input = item
                                .input
                                .ok_or_else(|| eyre!("OpenAI custom tool call omitted input"))?;
                            let call_id =
                                ToolCallId::try_new(call_id).map_err(|error| eyre!(error))?;
                            self.called_tool = true;
                            pending.push_back(ProviderStreamEvent::ScriptCall(ScriptCall {
                                call_id,
                                name,
                                input,
                                arguments_error: None,
                            }));
                        }
                        _ => {}
                    }
                }
            }
            "response.completed" => {
                let usage = event
                    .response
                    .and_then(|response| response.usage)
                    .and_then(normalize_usage)
                    .ok_or_else(|| eyre!("OpenAI response.completed event omitted usage"))?;
                pending.push_back(ProviderStreamEvent::Usage(usage));
                pending.push_back(ProviderStreamEvent::Finished {
                    stop_reason: if self.called_tool {
                        StopReason::ToolCall
                    } else {
                        StopReason::EndTurn
                    },
                });
                return Ok(StreamStatus::Complete);
            }
            "response.incomplete"
                if event
                    .response
                    .as_ref()
                    .and_then(|response| response.incomplete_details.as_ref())
                    .and_then(|details| details.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    == Some("max_output_tokens") =>
            {
                if let Some(usage) = event
                    .response
                    .and_then(|response| response.usage)
                    .and_then(normalize_usage)
                {
                    pending.push_back(ProviderStreamEvent::Usage(usage));
                }
                pending.push_back(ProviderStreamEvent::Finished {
                    stop_reason: StopReason::MaxOutputTokens,
                });
                return Ok(StreamStatus::Complete);
            }
            "response.failed" | "response.incomplete" => {
                let detail = event
                    .response
                    .map(format_response_failure)
                    .unwrap_or_else(|| String::from("no failure details were provided"));
                return Err(eyre!("OpenAI response stream failed: {detail}"));
            }
            _ => {}
        }
        Ok(StreamStatus::Continue)
    }
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

/// Reads token usage from a Responses API `usage` object.
pub fn usage_from_json(usage: &serde_json::Value) -> Option<kraai_types::TokenUsage> {
    serde_json::from_value::<ResponsesUsage>(usage.clone())
        .ok()
        .and_then(normalize_usage)
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
    use futures::{StreamExt, stream};

    #[tokio::test]
    async fn encrypted_reasoning_is_preserved_before_its_tool_call() {
        let payload = serde_json::json!({"type":"reasoning","id":"rs-1","encrypted_content":"opaque-ciphertext", "summary":[], "status":"completed"});
        let source = stream::iter(vec![
            Ok(SseEvent::Data(serde_json::json!({"type":"response.output_item.added","item":{"type":"reasoning","id":"rs-1"}}).to_string())),
            Ok(SseEvent::Data(serde_json::json!({"type":"response.output_item.done","item":payload}).to_string())),
            Ok(SseEvent::Data(r#"{"type":"response.output_item.done","item":{"type":"custom_tool_call","call_id":"call-1","name":"kraai_nushell","input":"ls"}}"#.into())),
            Ok(SseEvent::Data(r#"{"type":"response.completed","response":{"usage":{"input_tokens":100,"output_tokens":20,"output_tokens_details":{"reasoning_tokens":15}}}}"#.into())),
        ]).boxed();
        let events = adapt_responses_stream(source).collect::<Vec<_>>().await;
        assert_eq!(events.len(), 4);
        assert!(
            matches!(events.first(), Some(Ok(ProviderStreamEvent::Reasoning { payload: actual })) if actual == &payload)
        );
        assert!(matches!(
            events.get(1),
            Some(Ok(ProviderStreamEvent::ScriptCall(_)))
        ));
        assert!(
            matches!(events.get(2), Some(Ok(ProviderStreamEvent::Usage(usage))) if usage.reasoning_tokens == 15 && usage.input_tokens == 100)
        );
        assert!(matches!(
            events.get(3),
            Some(Ok(ProviderStreamEvent::Finished {
                stop_reason: StopReason::ToolCall
            }))
        ));
    }

    #[tokio::test]
    async fn max_output_token_incomplete_responses_finish_instead_of_failing() {
        let source = stream::iter(vec![
            Ok(SseEvent::Data(r#"{"type":"response.output_item.added","item":{"type":"message","id":"msg-1","phase":"final_answer"}}"#.into())),
            Ok(SseEvent::Data(r#"{"type":"response.output_text.delta","item_id":"msg-1","delta":"partial"}"#.into())),
            Ok(SseEvent::Data(r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":10,"output_tokens":5}}}"#.into())),
        ]).boxed();
        let events = adapt_responses_stream(source).collect::<Vec<_>>().await;
        assert!(events.iter().all(Result::is_ok), "{events:?}");
        assert!(matches!(
            events.last(),
            Some(Ok(ProviderStreamEvent::Finished {
                stop_reason: StopReason::MaxOutputTokens
            }))
        ));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Ok(ProviderStreamEvent::TextDelta { delta, .. }) if delta == "partial"))
        );
    }

    #[tokio::test]
    async fn reasoning_summaries_stream_as_display_deltas() {
        let source = stream::iter(vec![
            Ok(SseEvent::Data(r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs-1","delta":"Thinking"}"#.into())),
            Ok(SseEvent::Data(r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs-1","delta":""}"#.into())),
        ]).boxed();
        let events = adapt_responses_stream(source).collect::<Vec<_>>().await;
        let deltas: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Ok(ProviderStreamEvent::ReasoningDelta { item_id, delta }) => {
                    Some((item_id.as_str(), delta.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(deltas, [("rs-1", "Thinking")]);
    }

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

        let event = kraai_test_support::hang_guard(adapt_responses_stream(source).next())
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
        let error = kraai_test_support::hang_guard(events.next())
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
            Some(Ok(ProviderStreamEvent::ScriptCall(ScriptCall { call_id, name, input, arguments_error: None })))
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
