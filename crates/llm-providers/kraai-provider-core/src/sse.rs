use std::borrow::Cow;

use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use futures::{StreamExt, stream};
use reqwest::Response;
use tokio::sync::mpsc;

pub const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    Data(String),
    Done,
}

pub fn stream_sse_data(response: Response) -> BoxStream<'static, Result<SseEvent>> {
    let (tx, rx) = mpsc::channel(16);

    tokio::spawn(forward_sse_events(response.bytes_stream(), tx));

    Box::pin(stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|item| (item, rx))
    }))
}

async fn forward_sse_events<S, B>(mut bytes_stream: S, tx: mpsc::Sender<Result<SseEvent>>)
where
    S: futures::Stream<Item = std::result::Result<B, reqwest::Error>> + Unpin,
    B: AsRef<[u8]>,
{
    let mut buffer = Vec::new();
    let mut event_data = None;

    loop {
        let chunk = tokio::select! {
            _ = tx.closed() => return,
            chunk = bytes_stream.next() => chunk,
        };
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let _ = tx
                    .send(Err(eyre!(error).wrap_err(
                        crate::ProviderError::StreamInterrupted("HTTP response body failed".into()),
                    )))
                    .await;
                return;
            }
        };

        let chunk = chunk.as_ref();
        let scanned = buffer.len();
        buffer.extend_from_slice(chunk);

        let mut consumed = 0;
        let mut remaining = buffer.as_slice();
        for (offset, byte) in chunk.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            let line_end = scanned + offset + 1;
            let (line, tail) = remaining.split_at(line_end - consumed);
            if line.len() - 1 > MAX_SSE_EVENT_BYTES {
                let _ = tx
                    .send(Err(eyre!(
                        "SSE line exceeds the {MAX_SSE_EVENT_BYTES}-byte limit"
                    )))
                    .await;
                return;
            }
            consumed = line_end;
            remaining = tail;
            match process_line(&tx, line, &mut event_data).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            }
        }
        if consumed != 0 {
            drop(buffer.drain(..consumed));
        }

        if buffer.len() > MAX_SSE_EVENT_BYTES {
            let _ = tx
                .send(Err(eyre!(
                    "SSE line exceeds the {MAX_SSE_EVENT_BYTES}-byte limit"
                )))
                .await;
            return;
        }
    }

    if !buffer.is_empty() {
        match process_line(&tx, &buffer, &mut event_data).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                let _ = tx.send(Err(error)).await;
                return;
            }
        }
    }

    let _ = flush_event(&tx, &mut event_data).await;
}

async fn process_line(
    tx: &mpsc::Sender<Result<SseEvent>>,
    line: &[u8],
    event_data: &mut Option<String>,
) -> Result<bool> {
    let line = line.trim_suffix(b"\n").trim_suffix(b"\r");

    if line.is_empty() {
        return flush_event(tx, event_data).await;
    }

    let line = std::str::from_utf8(line)
        .map(Cow::Borrowed)
        .or_else(|_error| String::from_utf8(line.to_vec()).map(Cow::Owned))
        .map_err(|error| eyre!(error))?;
    if let Some(data) = line.strip_prefix("data:") {
        let data = data.trim_start();
        let event_bytes = event_data
            .as_ref()
            .map_or(0, |payload| payload.len().saturating_add(1))
            .saturating_add(data.len());
        if event_bytes > MAX_SSE_EVENT_BYTES {
            return Err(eyre!(
                "SSE event exceeds the {MAX_SSE_EVENT_BYTES}-byte limit"
            ));
        }
        if let Some(payload) = event_data {
            payload.push('\n');
            payload.push_str(data);
        } else {
            *event_data = Some(data.to_string());
        }
    }
    Ok(false)
}

async fn flush_event(
    tx: &mpsc::Sender<Result<SseEvent>>,
    event_data: &mut Option<String>,
) -> Result<bool> {
    let Some(payload) = event_data.take() else {
        return Ok(false);
    };

    if payload == "[DONE]" {
        tx.send(Ok(SseEvent::Done))
            .await
            .map_err(|error| eyre!(error.to_string()))?;
        return Ok(true);
    }

    tx.send(Ok(SseEvent::Data(payload)))
        .await
        .map_err(|error| eyre!(error.to_string()))?;
    Ok(false)
}

#[cfg(test)]
mod tests;
