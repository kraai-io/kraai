use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use futures::{StreamExt, stream};
use reqwest::Response;
use tokio::sync::mpsc;

pub fn stream_sse_data(response: Response) -> BoxStream<'static, Result<String>> {
    let (tx, rx) = mpsc::channel(16);

    tokio::spawn(forward_sse_events(response.bytes_stream(), tx));

    Box::pin(stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|item| (item, rx))
    }))
}

async fn forward_sse_events<S, B>(mut bytes_stream: S, tx: mpsc::Sender<Result<String>>)
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
            if process_line(&tx, line, &mut event_lines).await.is_err() {
                return;
            }
        }
    }

    if !buffer.is_empty() && process_line(&tx, buffer, &mut event_lines).await.is_err() {
        return;
    }

    let _ = flush_event(&tx, &mut event_lines).await;
}

async fn process_line(
    tx: &mpsc::Sender<Result<String>>,
    mut line: Vec<u8>,
    event_lines: &mut Vec<String>,
) -> Result<()> {
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
    Ok(())
}

async fn flush_event(
    tx: &mpsc::Sender<Result<String>>,
    event_lines: &mut Vec<String>,
) -> Result<()> {
    if event_lines.is_empty() {
        return Ok(());
    }

    let payload = event_lines.join("\n");
    event_lines.clear();

    if payload == "[DONE]" {
        return Ok(());
    }

    tx.send(Ok(payload))
        .await
        .map_err(|error| eyre!(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    #[tokio::test]
    async fn emits_final_event_without_trailing_newline() {
        let (tx, mut rx) = mpsc::channel(4);

        forward_sse_events(
            stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(
                b"data: final payload".to_vec(),
            )]),
            tx,
        )
        .await;

        assert_eq!(rx.recv().await.unwrap().unwrap(), "final payload");
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn emits_final_event_when_last_line_is_split_across_chunks() {
        let (tx, mut rx) = mpsc::channel(4);

        forward_sse_events(
            stream::iter(vec![
                Ok::<Vec<u8>, reqwest::Error>(b"data: split".to_vec()),
                Ok::<Vec<u8>, reqwest::Error>(b" payload".to_vec()),
            ]),
            tx,
        )
        .await;

        assert_eq!(rx.recv().await.unwrap().unwrap(), "split payload");
        assert!(rx.recv().await.is_none());
    }
}
