use std::borrow::Cow;

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
                let _ = tx.send(Err(eyre!(error))).await;
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
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);

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
#[expect(
    clippy::expect_used,
    clippy::panic_in_result_fn,
    reason = "fallible channel setup is combined with direct assertions"
)]
mod tests {
    use super::*;
    use futures::stream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Poll;
    use std::time::Duration;

    async fn collect_chunks(payload: &[u8], chunk_size: usize) -> Vec<Result<SseEvent>> {
        let (tx, mut rx) = mpsc::channel(16);
        let chunks = payload.chunks(chunk_size).map(Ok::<_, reqwest::Error>);
        forward_sse_events(stream::iter(chunks), tx).await;
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn fragmented_empty_data_and_whitespace_keep_event_boundaries() -> Result<()> {
        let payload = "\n: keepalive\r\ndata:\n\ndata:\ndata:\n\n\
            data: \u{2003}\t hello  \r\nignored: value\n\ndata: final\r";
        for chunk_size in 1..=payload.len() {
            let events = collect_chunks(payload.as_bytes(), chunk_size)
                .await
                .into_iter()
                .collect::<Result<Vec<_>>>()?;
            assert_eq!(
                events,
                vec![
                    SseEvent::Data(String::new()),
                    SseEvent::Data(String::from("\n")),
                    SseEvent::Data(String::from("hello  ")),
                    SseEvent::Data(String::from("final")),
                ],
                "chunk size {chunk_size}",
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn done_discards_invalid_and_oversized_trailing_data() -> Result<()> {
        let mut payload = b"data: first\n\ndata: [DONE]\n\n".to_vec();
        payload.extend(std::iter::repeat_n(0xff, MAX_SSE_EVENT_BYTES + 1));
        for chunk_size in [1, 4096, payload.len()] {
            let events = collect_chunks(&payload, chunk_size)
                .await
                .into_iter()
                .collect::<Result<Vec<_>>>()?;
            assert_eq!(
                events,
                vec![SseEvent::Data(String::from("first")), SseEvent::Done],
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn invalid_utf8_keeps_original_error_bytes_after_line_ending_removal() {
        for ending in [b"\r\n".as_slice(), b"\r", b""] {
            let mut payload = b": invalid \xff".to_vec();
            payload.extend_from_slice(ending);
            for chunk_size in 1..=payload.len() {
                let events = collect_chunks(&payload, chunk_size).await;
                assert_eq!(events.len(), 1);
                let error = events
                    .first()
                    .expect("one error")
                    .as_ref()
                    .expect_err("invalid UTF-8 should fail");
                let error = error
                    .downcast_ref::<std::string::FromUtf8Error>()
                    .expect("original owned UTF-8 error type");
                assert_eq!(error.as_bytes(), b": invalid \xff");
            }
        }
    }

    #[tokio::test]
    async fn line_limit_counts_carriage_return_and_excludes_newline() -> Result<()> {
        let data = "x".repeat(MAX_SSE_EVENT_BYTES - "data:".len());
        for ending in ["", "\n", "\r\n"] {
            let payload = format!("data:{data}{ending}");
            for chunk_size in [7, 4096, payload.len()] {
                let mut events = collect_chunks(payload.as_bytes(), chunk_size).await;
                assert_eq!(events.len(), 1);
                let event = events.pop().expect("one event");
                if ending == "\r\n" {
                    assert_eq!(
                        event.expect_err("CR exceeds line limit").to_string(),
                        format!("SSE line exceeds the {MAX_SSE_EVENT_BYTES}-byte limit"),
                    );
                } else {
                    assert_eq!(event?, SseEvent::Data(data.clone()));
                }
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn multiline_limit_counts_separators_and_resets_after_dispatch() -> Result<()> {
        let first = "x".repeat(MAX_SSE_EVENT_BYTES / 2);
        let second = "y".repeat(MAX_SSE_EVENT_BYTES - first.len() - 1);
        for extra in ["", "z"] {
            let payload = format!("data:{first}\ndata:{second}{extra}\n\ndata:\n\n");
            for chunk_size in [31, 4096, payload.len()] {
                let events = collect_chunks(payload.as_bytes(), chunk_size).await;
                if extra.is_empty() {
                    let events = events.into_iter().collect::<Result<Vec<_>>>()?;
                    assert_eq!(
                        events,
                        vec![
                            SseEvent::Data(format!("{first}\n{second}")),
                            SseEvent::Data(String::new()),
                        ],
                    );
                } else {
                    assert_eq!(events.len(), 1);
                    assert_eq!(
                        events
                            .into_iter()
                            .next()
                            .expect("one error")
                            .expect_err("separator exceeds event limit")
                            .to_string(),
                        format!("SSE event exceeds the {MAX_SSE_EVENT_BYTES}-byte limit"),
                    );
                }
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn dropping_receiver_releases_pending_transport() -> Result<()> {
        struct DropSignal(Arc<AtomicBool>);

        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let signal = DropSignal(Arc::clone(&dropped));
        let source = stream::poll_fn(move |_| {
            let _ = &signal;
            Poll::<Option<std::result::Result<Vec<u8>, reqwest::Error>>>::Pending
        });
        let (tx, rx) = mpsc::channel(4);
        let forwarding = forward_sse_events(source, tx);
        tokio::pin!(forwarding);

        assert!(futures::poll!(&mut forwarding).is_pending());
        assert!(!dropped.load(Ordering::Acquire));
        drop(rx);
        tokio::time::timeout(Duration::from_secs(1), forwarding).await?;

        assert!(dropped.load(Ordering::Acquire));
        Ok(())
    }

    #[tokio::test]
    async fn coalesced_and_fragmented_short_lines_emit_the_same_events() -> Result<()> {
        let mut payload = String::from(": keepalive\r\n");
        let mut expected = Vec::new();
        for index in 0..128 {
            payload.push_str(&format!("data: {index}\r\ndata: continuation\r\n\r\n"));
            expected.push(SseEvent::Data(format!("{index}\ncontinuation")));
        }
        payload.push_str("data: café");
        expected.push(SseEvent::Data(String::from("café")));

        for chunk_size in [1, 7, payload.len()] {
            let (tx, mut rx) = mpsc::channel(expected.len());
            let chunks = payload
                .as_bytes()
                .chunks(chunk_size)
                .map(|chunk| Ok::<_, reqwest::Error>(chunk.to_vec()));
            forward_sse_events(stream::iter(chunks), tx).await;

            let mut actual = Vec::new();
            while let Some(event) = rx.recv().await {
                actual.push(event?);
            }
            assert_eq!(actual, expected, "chunk size {chunk_size}");
        }
        Ok(())
    }

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
