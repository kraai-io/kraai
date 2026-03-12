use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use futures::{StreamExt, stream};
use reqwest::Response;
use tokio::sync::mpsc;

pub fn stream_sse_data(response: Response) -> BoxStream<'static, Result<String>> {
    let (tx, rx) = mpsc::channel(16);

    tokio::spawn(async move {
        let mut bytes_stream = response.bytes_stream();
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

            buffer.extend_from_slice(&chunk);

            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let mut line = buffer.drain(..=position).collect::<Vec<_>>();
                if matches!(line.last(), Some(b'\n')) {
                    line.pop();
                }
                if matches!(line.last(), Some(b'\r')) {
                    line.pop();
                }

                if line.is_empty() {
                    if flush_event(&tx, &mut event_lines).await.is_err() {
                        return;
                    }
                    continue;
                }

                let line = match String::from_utf8(line) {
                    Ok(line) => line,
                    Err(error) => {
                        let _ = tx.send(Err(eyre!(error))).await;
                        return;
                    }
                };

                if let Some(data) = line.strip_prefix("data:") {
                    event_lines.push(data.trim_start().to_string());
                }
            }
        }

        if !event_lines.is_empty() {
            let _ = flush_event(&tx, &mut event_lines).await;
        }
    });

    Box::pin(stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|item| (item, rx))
    }))
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
