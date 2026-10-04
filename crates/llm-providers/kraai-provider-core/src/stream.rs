use std::collections::VecDeque;

use color_eyre::eyre::Result;
use futures::{StreamExt, stream, stream::BoxStream};
use kraai_types::{AssistantPhase, TokenUsage, ToolCallId};

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
    TextDelta {
        item_id: String,
        phase: AssistantPhase,
        delta: String,
    },
    ScriptCall {
        call_id: ToolCallId,
        name: String,
        input: String,
    },
    Usage(TokenUsage),
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
