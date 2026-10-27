use std::collections::VecDeque;

use color_eyre::eyre::Result;
use futures::{StreamExt, stream, stream::BoxStream};
use kraai_types::{AssistantPhase, ScriptCall, TokenUsage};

use crate::{ProviderError, SseEvent};

pub enum StreamStatus {
    Continue,
    Complete,
}

pub fn adapt_provider_stream<F>(
    source: BoxStream<'static, Result<SseEvent>>,
    interrupted_message: &'static str,
    decode: F,
) -> BoxStream<'static, Result<ProviderStreamEvent>>
where
    F: FnMut(SseEvent, &mut VecDeque<ProviderStreamEvent>) -> Result<StreamStatus> + Send + 'static,
{
    stream::unfold(
        (source, decode, VecDeque::new(), None, false),
        move |(mut source, mut decode, mut pending, mut failure, mut finished)| async move {
            loop {
                if let Some(event) = pending.pop_front() {
                    return Some((Ok(event), (source, decode, pending, failure, finished)));
                }
                if let Some(error) = failure.take() {
                    return Some((Err(error), (source, decode, pending, None, true)));
                }
                if finished {
                    return None;
                }
                let result = match source.next().await {
                    Some(Ok(event)) => decode(event, &mut pending),
                    Some(Err(error)) => Err(error),
                    None => {
                        Err(ProviderError::StreamInterrupted(interrupted_message.into()).into())
                    }
                };
                match result {
                    Ok(StreamStatus::Continue) => {}
                    Ok(StreamStatus::Complete) => finished = true,
                    Err(error) => failure = Some(error),
                }
            }
        },
    )
    .boxed()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderStreamEvent {
    Compaction {
        payload: serde_json::Value,
    },
    Reasoning {
        payload: serde_json::Value,
    },
    /// Readable reasoning text for display while the model thinks. The opaque `Reasoning` item,
    /// not these deltas, is what later requests replay.
    ReasoningDelta {
        item_id: String,
        delta: String,
    },
    TextDelta {
        item_id: String,
        phase: AssistantPhase,
        delta: String,
    },
    ScriptCall(ScriptCall),
    Usage(TokenUsage),
    /// Why the provider ended the response; sent once, after its content.
    Finished {
        stop_reason: StopReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolCall,
    /// The response reached the output token limit; its content is complete as far as it goes.
    MaxOutputTokens,
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
