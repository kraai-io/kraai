use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use futures::{StreamExt, stream};
use reqwest::Response;
use tokio::sync::mpsc;

/// Maximum size of one SSE line or accumulated multi-line event payload.
pub const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    Data(String),
    Done,
}

/// Convert an HTTP server-sent-events response into decoded `data:` events.
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
    let mut event_lines: Vec<String> = Vec::new();
    let mut event_bytes = 0_usize;

    while let Some(chunk) = bytes_stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let _ = tx.send(Err(eyre!(error))).await;
                return;
            }
        };

        buffer.extend_from_slice(chunk.as_ref());

        while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
            if position > MAX_SSE_EVENT_BYTES {
                let _ = tx
                    .send(Err(eyre!(
                        "SSE line exceeds the {MAX_SSE_EVENT_BYTES}-byte limit"
                    )))
                    .await;
                return;
            }
            let line = buffer.drain(..=position).collect::<Vec<_>>();
            match process_line(&tx, line, &mut event_lines, &mut event_bytes).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            }
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
        match process_line(&tx, buffer, &mut event_lines, &mut event_bytes).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                let _ = tx.send(Err(error)).await;
                return;
            }
        }
    }

    let _ = flush_event(&tx, &mut event_lines).await;
}

async fn process_line(
    tx: &mpsc::Sender<Result<SseEvent>>,
    mut line: Vec<u8>,
    event_lines: &mut Vec<String>,
    event_bytes: &mut usize,
) -> Result<bool> {
    if matches!(line.last(), Some(b'\n')) {
        line.pop();
    }
    if matches!(line.last(), Some(b'\r')) {
        line.pop();
    }

    if line.is_empty() {
        let result = flush_event(tx, event_lines).await;
        if result.is_ok() {
            *event_bytes = 0;
        }
        return result;
    }

    let line = String::from_utf8(line).map_err(|error| eyre!(error))?;
    if let Some(data) = line.strip_prefix("data:") {
        let data = data.trim_start();
        *event_bytes = event_bytes
            .saturating_add(usize::from(!event_lines.is_empty()))
            .saturating_add(data.len());
        if *event_bytes > MAX_SSE_EVENT_BYTES {
            return Err(eyre!(
                "SSE event exceeds the {MAX_SSE_EVENT_BYTES}-byte limit"
            ));
        }
        event_lines.push(data.to_string());
    }
    Ok(false)
}

async fn flush_event(
    tx: &mpsc::Sender<Result<SseEvent>>,
    event_lines: &mut Vec<String>,
) -> Result<bool> {
    if event_lines.is_empty() {
        return Ok(false);
    }

    let payload = event_lines.join("\n");
    event_lines.clear();

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
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible channel setup is combined with direct assertions"
)]
mod tests {
    use super::*;
    use futures::stream;
    use std::time::Duration;

    #[tokio::test]
    async fn emits_final_event_without_trailing_newline() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(4);

        forward_sse_events(
            stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(
                b"data: final payload".to_vec(),
            )]),
            tx,
        )
        .await;

        let event = rx
            .recv()
            .await
            .ok_or_else(|| eyre!("missing final event"))??;
        assert_eq!(event, SseEvent::Data(String::from("final payload")));
        assert!(rx.recv().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn emits_final_event_when_last_line_is_split_across_chunks() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(4);

        forward_sse_events(
            stream::iter(vec![
                Ok::<Vec<u8>, reqwest::Error>(b"data: split".to_vec()),
                Ok::<Vec<u8>, reqwest::Error>(b" payload".to_vec()),
            ]),
            tx,
        )
        .await;

        let event = rx
            .recv()
            .await
            .ok_or_else(|| eyre!("missing split event"))??;
        assert_eq!(event, SseEvent::Data(String::from("split payload")));
        assert!(rx.recv().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn reports_invalid_utf8_in_newline_delimited_line() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(4);

        forward_sse_events(
            stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(vec![
                b'd', b'a', b't', b'a', b':', b' ', 0xff, b'\n',
            ])]),
            tx,
        )
        .await;

        let error = rx.recv().await.ok_or_else(|| eyre!("missing error"))?;
        assert!(error.is_err());
        assert!(rx.recv().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn reports_invalid_utf8_in_final_unterminated_line() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(4);

        forward_sse_events(
            stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(vec![
                b'd', b'a', b't', b'a', b':', b' ', 0xff,
            ])]),
            tx,
        )
        .await;

        let error = rx.recv().await.ok_or_else(|| eyre!("missing error"))?;
        assert!(error.is_err());
        assert!(rx.recv().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn emits_done_and_stops_without_waiting_for_transport_eof() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(4);
        let source = stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(
            b"data: [DONE]\n\n".to_vec(),
        )])
        .chain(stream::pending());

        tokio::time::timeout(Duration::from_secs(1), forward_sse_events(source, tx)).await?;

        assert!(matches!(rx.recv().await, Some(Ok(SseEvent::Done))));
        assert!(rx.recv().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn rejects_oversized_sse_lines() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(4);
        let mut line = b"data: ".to_vec();
        line.extend(std::iter::repeat_n(b'x', MAX_SSE_EVENT_BYTES + 1));

        forward_sse_events(stream::iter(vec![Ok::<_, reqwest::Error>(line)]), tx).await;

        let error = rx.recv().await.ok_or_else(|| eyre!("missing error"))?;
        assert!(
            error
                .expect_err("oversized line should fail")
                .to_string()
                .contains("SSE line exceeds")
        );
        assert!(rx.recv().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn rejects_oversized_multiline_sse_events() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(4);
        let half = "x".repeat(MAX_SSE_EVENT_BYTES / 2);
        let payload = format!("data: {half}\ndata: {half}\n\n").into_bytes();

        forward_sse_events(stream::iter(vec![Ok::<_, reqwest::Error>(payload)]), tx).await;

        let error = rx.recv().await.ok_or_else(|| eyre!("missing error"))?;
        assert!(
            error
                .expect_err("oversized event should fail")
                .to_string()
                .contains("SSE event exceeds")
        );
        assert!(rx.recv().await.is_none());
        Ok(())
    }
}
