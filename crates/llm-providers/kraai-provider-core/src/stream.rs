use std::collections::VecDeque;

use color_eyre::eyre::Result;
use futures::{StreamExt, stream, stream::BoxStream};
use kraai_types::{AssistantPhase, ScriptCall, TokenUsage};

use crate::{ProviderError, SseEvent};

pub enum StreamStatus {
    Continue,
    Complete,
}

/// `decode` gets `None` at EOF. Returning `Continue` there reports the stream as interrupted.
pub fn adapt_provider_stream<F>(
    source: BoxStream<'static, Result<SseEvent>>,
    interrupted_message: &'static str,
    mut decode: F,
) -> BoxStream<'static, Result<ProviderStreamEvent>>
where
    F: FnMut(Option<SseEvent>, &mut VecDeque<ProviderStreamEvent>) -> Result<StreamStatus>
        + Send
        + 'static,
{
    let decode = move |event: Option<SseEvent>, pending: &mut VecDeque<ProviderStreamEvent>| {
        let at_eof = event.is_none();
        match decode(event, pending)? {
            StreamStatus::Continue if at_eof => {
                Err(ProviderError::StreamInterrupted(interrupted_message.into()).into())
            }
            status @ (StreamStatus::Continue | StreamStatus::Complete) => Ok(status),
        }
    };
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
                    Some(Ok(event)) => decode(Some(event), &mut pending),
                    Some(Err(error)) => Err(error),
                    None => decode(None, &mut pending),
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
    /// The opaque `Reasoning` item, not these deltas, is what later requests replay.
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
    /// Sent once, after the response's content.
    Finished {
        stop_reason: StopReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolCall,
    MaxOutputTokens,
}

#[cfg(test)]
mod tests;
