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
                                rewrite: None,
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
                let code = event
                    .response
                    .as_ref()
                    .and_then(|response| response.error.as_ref())
                    .and_then(|error| error.code.clone());
                let detail = event
                    .response
                    .map(format_response_failure)
                    .unwrap_or_else(|| String::from("no failure details were provided"));
                return Err(ProviderError::from_stream_error(
                    code.as_deref(),
                    format!("OpenAI response stream failed: {detail}"),
                )
                .into());
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
mod tests;
