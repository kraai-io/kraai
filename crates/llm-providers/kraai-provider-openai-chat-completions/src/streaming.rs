use std::collections::VecDeque;

use color_eyre::eyre::{Result, ensure, eyre};
use futures::stream::BoxStream;
use kraai_provider_core::{ProviderStreamEvent, SseEvent, StreamStatus, adapt_provider_stream};
use kraai_types::{AssistantPhase, ToolCallId};

use crate::usage::normalize_usage;
use crate::wire::{ChatCompletionChunk, ScriptArguments, ToolCallDelta};

const MAX_TOOL_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct PendingCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct Completion {
    call: Option<PendingCall>,
    reasoning: crate::reasoning::Reasoning,
    finish_reason: Option<String>,
}

impl Completion {
    fn append_call(&mut self, delta: ToolCallDelta, tool_name: Option<&str>) -> Result<()> {
        ensure!(
            tool_name.is_some(),
            "Provider called a tool when tools were disabled"
        );
        ensure!(delta.index == 0, "Provider emitted more than one tool call");
        ensure!(
            delta.kind.as_deref().is_none_or(|kind| kind == "function"),
            "Unsupported tool call type"
        );
        let call = self.call.get_or_insert_with(PendingCall::default);
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
            return Err(eyre!("Chat completions stream failed: {}", error.message));
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
            ensure!(
                self.finish_reason.is_none(),
                "Provider sent a choice after completion"
            );
            if let Some(refusal) = choice.delta.refusal.filter(|text| !text.is_empty()) {
                return Err(eyre!("Provider refused the request: {refusal}"));
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
            ensure!(
                tool_calls.len() <= 1,
                "Provider emitted more than one tool call"
            );
            for delta in tool_calls {
                self.append_call(delta, tool_name)?;
            }
            if let Some(reason) = choice.finish_reason {
                ensure!(
                    matches!(reason.as_str(), "stop" | "tool_calls"),
                    "Chat completions response did not complete successfully: {reason}"
                );
                ensure!(
                    (reason == "tool_calls") == self.call.is_some(),
                    "Completion finish reason does not match its tool calls"
                );
                self.finish_reason = Some(reason);
                let call = self.finish_call(tool_name)?;
                let reasoning = std::mem::take(&mut self.reasoning);
                if !reasoning.is_empty() {
                    pending.push_back(ProviderStreamEvent::Reasoning {
                        payload: serde_json::to_value(reasoning)?,
                    });
                }
                if let Some(call) = call {
                    pending.push_back(call);
                }
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

    fn finish_call(&mut self, tool_name: Option<&str>) -> Result<Option<ProviderStreamEvent>> {
        let Some(call) = self.call.take() else {
            return Ok(None);
        };
        ensure!(
            Some(call.name.as_str()) == tool_name,
            "Provider called unexpected tool '{}'",
            call.name
        );
        let id = call.id.ok_or_else(|| eyre!("Tool call omitted ID"))?;
        let call_id = ToolCallId::try_new(id).map_err(|error| eyre!(error))?;
        let arguments: ScriptArguments = serde_json::from_str(&call.arguments)?;
        Ok(Some(ProviderStreamEvent::ScriptCall {
            call_id,
            name: call.name,
            input: arguments.input,
        }))
    }
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
