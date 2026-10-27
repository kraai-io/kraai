use std::collections::VecDeque;

use color_eyre::eyre::{Result, ensure, eyre};
use futures::stream::BoxStream;
use kraai_provider_core::{
    ProviderStreamEvent, SseEvent, StopReason, StreamStatus, adapt_provider_stream,
};
use kraai_types::{AssistantPhase, ScriptCall, ToolCallId};

use crate::usage::normalize_usage;
use crate::wire::{ChatCompletionChunk, ScriptArguments, ToolCallDelta};

const MAX_TOOL_BYTES: usize = 1024 * 1024;
const MAX_TOOL_CALLS: usize = 16;

#[derive(Default)]
struct PendingCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct Completion {
    /// Calls by their stream index; models that ignore `parallel_tool_calls` can send several.
    calls: Vec<PendingCall>,
    reasoning: crate::reasoning::Reasoning,
    finish_reason: Option<String>,
}

impl Completion {
    fn append_call(&mut self, delta: ToolCallDelta, tool_name: Option<&str>) -> Result<()> {
        ensure!(
            tool_name.is_some(),
            "Provider called a tool when tools were disabled"
        );
        ensure!(
            delta.index < MAX_TOOL_CALLS,
            "Provider emitted more than {MAX_TOOL_CALLS} tool calls"
        );
        ensure!(
            delta.kind.as_deref().is_none_or(|kind| kind == "function"),
            "Unsupported tool call type"
        );
        if self.calls.len() <= delta.index {
            self.calls
                .resize_with(delta.index + 1, PendingCall::default);
        }
        let Some(call) = self.calls.get_mut(delta.index) else {
            return Err(eyre!("Tool call index is out of range"));
        };
        if let Some(id) = delta.id {
            ensure!(
                id.len() <= MAX_TOOL_BYTES,
                "Tool call ID exceeds size limit"
            );
            ensure!(
                call.id.as_ref().is_none_or(|previous| previous == &id),
                "Tool call ID changed during streaming"
            );
            call.id = Some(id);
        }
        if let Some(function) = delta.function {
            for (target, fragment) in [
                (&mut call.name, function.name),
                (&mut call.arguments, function.arguments),
            ] {
                if let Some(fragment) = fragment {
                    ensure!(
                        target.len().saturating_add(fragment.len()) <= MAX_TOOL_BYTES,
                        "Tool call exceeds size limit"
                    );
                    target.push_str(&fragment);
                }
            }
        }
        Ok(())
    }

    fn ingest(
        &mut self,
        chunk: ChatCompletionChunk,
        tool_name: Option<&str>,
        reported_costs: bool,
        pending: &mut VecDeque<ProviderStreamEvent>,
    ) -> Result<()> {
        if let Some(error) = chunk.error {
            let code = match &error.code {
                Some(serde_json::Value::String(code)) => Some(code.clone()),
                Some(serde_json::Value::Number(code)) if code.as_u64() == Some(429) => {
                    Some(String::from("rate_limit_exceeded"))
                }
                Some(serde_json::Value::Number(code))
                    if code.as_u64().is_some_and(|code| code >= 500) =>
                {
                    Some(String::from("server_error"))
                }
                _ => None,
            };
            return Err(kraai_provider_core::ProviderError::from_stream_error(
                code.as_deref(),
                format!("Chat completions stream failed: {}", error.message),
            )
            .into());
        }
        ensure!(
            chunk.choices.len() <= 1,
            "Provider returned multiple completion choices"
        );
        for choice in chunk.choices {
            ensure!(
                choice.index == 0,
                "Provider returned an unexpected completion choice"
            );
            if let Some(finished) = &self.finish_reason {
                ensure!(
                    choice.delta.is_empty()
                        && choice
                            .finish_reason
                            .as_ref()
                            .is_none_or(|reason| reason == finished),
                    "Provider sent a choice after completion"
                );
                continue;
            }
            if let Some(refusal) = choice.delta.refusal.filter(|text| !text.is_empty()) {
                return Err(eyre!("Provider refused the request: {refusal}"));
            }
            if let Some(delta) = choice.delta.reasoning.display_text() {
                pending.push_back(ProviderStreamEvent::ReasoningDelta {
                    item_id: String::from("chat-completions-reasoning"),
                    delta: delta.to_string(),
                });
            }
            self.reasoning.append(choice.delta.reasoning)?;
            if let Some(delta) = choice.delta.content.filter(|text| !text.is_empty()) {
                pending.push_back(ProviderStreamEvent::TextDelta {
                    item_id: String::from("chat-completions-message"),
                    phase: AssistantPhase::FinalAnswer,
                    delta,
                });
            }
            let tool_calls = choice.delta.tool_calls.unwrap_or_default();
            let mut indexes: Vec<_> = tool_calls.iter().map(|delta| delta.index).collect();
            indexes.sort_unstable();
            indexes.dedup();
            ensure!(
                indexes.len() == tool_calls.len(),
                "Provider sent two fragments for one tool call in a single chunk"
            );
            for delta in tool_calls {
                self.append_call(delta, tool_name)?;
            }
            if let Some(reason) = choice.finish_reason {
                let stop_reason = match reason.as_str() {
                    "stop" => StopReason::EndTurn,
                    "tool_calls" => StopReason::ToolCall,
                    "length" => StopReason::MaxOutputTokens,
                    _ => {
                        return Err(eyre!(
                            "Chat completions response did not complete successfully: {reason}"
                        ));
                    }
                };
                if stop_reason == StopReason::MaxOutputTokens {
                    self.calls.clear();
                }
                ensure!(
                    (stop_reason == StopReason::ToolCall) == !self.calls.is_empty(),
                    "Completion finish reason does not match its tool calls"
                );
                self.finish_reason = Some(reason);
                let calls = self.finish_calls(tool_name)?;
                let reasoning = std::mem::take(&mut self.reasoning);
                if !reasoning.is_empty() {
                    pending.push_back(ProviderStreamEvent::Reasoning {
                        payload: serde_json::to_value(reasoning)?,
                    });
                }
                pending.extend(calls);
                pending.push_back(ProviderStreamEvent::Finished { stop_reason });
            }
        }
        if let Some(usage) = chunk
            .usage
            .and_then(|usage| normalize_usage(usage, reported_costs))
        {
            pending.push_back(ProviderStreamEvent::Usage(usage));
        }
        Ok(())
    }

    fn finish_calls(&mut self, tool_name: Option<&str>) -> Result<Vec<ProviderStreamEvent>> {
        let calls: Vec<_> = std::mem::take(&mut self.calls)
            .into_iter()
            .map(|call| finish_call(call, tool_name))
            .collect::<Result<_>>()?;
        let mut ids: Vec<_> = calls
            .iter()
            .filter_map(|call| match call {
                ProviderStreamEvent::ScriptCall(ScriptCall { call_id, .. }) => Some(call_id),
                _ => None,
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ensure!(ids.len() == calls.len(), "Provider reused a tool call ID");
        Ok(calls)
    }
}

fn finish_call(call: PendingCall, tool_name: Option<&str>) -> Result<ProviderStreamEvent> {
    let tool_name =
        tool_name.ok_or_else(|| eyre!("Provider called a tool when tools were disabled"))?;
    let id = call.id.ok_or_else(|| eyre!("Tool call omitted ID"))?;
    let call_id = ToolCallId::try_new(id).map_err(|error| eyre!(error))?;
    let arguments_error = if call.name == tool_name {
        match serde_json::from_str::<ScriptArguments>(&call.arguments) {
            Ok(arguments) => {
                return Ok(ProviderStreamEvent::ScriptCall(ScriptCall {
                    call_id,
                    name: call.name,
                    input: arguments.input,
                    arguments_error: None,
                }));
            }
            Err(error) => {
                format!("arguments must be a JSON object with one string field \"input\": {error}")
            }
        }
    } else {
        format!(
            "unknown tool '{}'; the only tool is '{tool_name}'",
            call.name
        )
    };
    Ok(ProviderStreamEvent::ScriptCall(ScriptCall {
        call_id,
        name: call.name,
        input: call.arguments,
        arguments_error: Some(arguments_error),
    }))
}

pub(super) fn adapt_chat_completion_stream(
    source: BoxStream<'static, Result<SseEvent>>,
    tool_name: Option<String>,
    reported_costs: bool,
) -> BoxStream<'static, Result<ProviderStreamEvent>> {
    let mut completion = Completion::default();
    adapt_provider_stream(
        source,
        "Chat completions stream ended before the [DONE] marker",
        move |event, pending| match event {
            SseEvent::Data(payload) => {
                completion.ingest(
                    serde_json::from_str(&payload)?,
                    tool_name.as_deref(),
                    reported_costs,
                    pending,
                )?;
                Ok(StreamStatus::Continue)
            }
            SseEvent::Done => {
                ensure!(
                    completion.finish_reason.is_some(),
                    "Chat completions stream omitted a finish reason"
                );
                Ok(StreamStatus::Complete)
            }
        },
    )
}

#[cfg(test)]
#[path = "streaming_tests.rs"]
mod tests;
