use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use futures::{StreamExt, stream};
use reqwest::Response;
use tokio::sync::mpsc;

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
            let line = buffer.drain(..=position).collect::<Vec<_>>();
            match process_line(&tx, line, &mut event_lines).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            }
        }
    }

    if !buffer.is_empty() {
        match process_line(&tx, buffer, &mut event_lines).await {
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
) -> Result<bool> {
    if matches!(line.last(), Some(b'\n')) {
        line.pop();
    }
    if matches!(line.last(), Some(b'\r')) {
        line.pop();
    }

    if line.is_empty() {
        return flush_event(tx, event_lines).await;
    }

    let line = String::from_utf8(line).map_err(|error| eyre!(error))?;
    if let Some(data) = line.strip_prefix("data:") {
        event_lines.push(data.trim_start().to_string());
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
}
