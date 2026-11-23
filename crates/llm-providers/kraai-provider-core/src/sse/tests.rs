use super::*;
use std::mem::DropGuard;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

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
async fn fragmented_empty_data_and_whitespace_keep_event_boundaries() {
    let payload = "\n: keepalive\r\ndata:\n\ndata:\ndata:\n\n\
            data: \u{2003}\t hello  \r\nignored: value\n\ndata: final\r";
    for chunk_size in 1..=payload.len() {
        let events = collect_chunks(payload.as_bytes(), chunk_size)
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
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
}

#[tokio::test]
async fn done_discards_invalid_and_oversized_trailing_data() {
    let mut payload = b"data: first\n\ndata: [DONE]\n\n".to_vec();
    payload.extend(std::iter::repeat_n(0xff, MAX_SSE_EVENT_BYTES + 1));
    for chunk_size in [1, 4096, payload.len()] {
        let events = collect_chunks(&payload, chunk_size)
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            events,
            vec![SseEvent::Data(String::from("first")), SseEvent::Done],
        );
    }
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
async fn line_limit_counts_carriage_return_and_excludes_newline() {
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
                assert_eq!(event.unwrap(), SseEvent::Data(data.clone()));
            }
        }
    }
}

#[tokio::test]
async fn multiline_limit_counts_separators_and_resets_after_dispatch() {
    let first = "x".repeat(MAX_SSE_EVENT_BYTES / 2);
    let second = "y".repeat(MAX_SSE_EVENT_BYTES - first.len() - 1);
    for extra in ["", "z"] {
        let payload = format!("data:{first}\ndata:{second}{extra}\n\ndata:\n\n");
        for chunk_size in [31, 4096, payload.len()] {
            let events = collect_chunks(payload.as_bytes(), chunk_size).await;
            if extra.is_empty() {
                let events = events.into_iter().collect::<Result<Vec<_>>>().unwrap();
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
}

#[tokio::test]
async fn dropping_receiver_releases_pending_transport() {
    let dropped = Arc::new(AtomicBool::new(false));
    let signal = DropGuard::new(Arc::clone(&dropped), |dropped| {
        dropped.store(true, Ordering::Release);
    });
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
    kraai_test_support::hang_guard(forwarding).await.unwrap();

    assert!(dropped.load(Ordering::Acquire));
}

#[tokio::test]
async fn coalesced_and_fragmented_short_lines_emit_the_same_events() {
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
            actual.push(event.unwrap());
        }
        assert_eq!(actual, expected, "chunk size {chunk_size}");
    }
}

#[tokio::test]
async fn emits_done_and_stops_without_waiting_for_transport_eof() {
    let (tx, mut rx) = mpsc::channel(4);
    let source = stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(
        b"data: [DONE]\n\n".to_vec(),
    )])
    .chain(stream::pending());

    kraai_test_support::hang_guard(forward_sse_events(source, tx))
        .await
        .unwrap();

    assert!(matches!(rx.recv().await, Some(Ok(SseEvent::Done))));
    assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn rejects_oversized_sse_lines() {
    let (tx, mut rx) = mpsc::channel(4);
    let mut line = b"data: ".to_vec();
    line.extend(std::iter::repeat_n(b'x', MAX_SSE_EVENT_BYTES + 1));

    forward_sse_events(stream::iter(vec![Ok::<_, reqwest::Error>(line)]), tx).await;

    let error = rx.recv().await.expect("missing error");
    assert!(
        error
            .expect_err("oversized line should fail")
            .to_string()
            .contains("SSE line exceeds")
    );
    assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn streaming_client_times_out_when_body_stalls() {
    use kraai_io::http::{HttpTimeouts, client_builder};
    use kraai_test_support::http;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut listener = http::Listener::bind(http::PATH).await.unwrap();
    let url = listener.url().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nd\r\ndata: hello\n\n\r\n",
            )
            .await
            .unwrap();
        stream.flush().await.unwrap();
        std::future::pending::<()>().await;
    });
    let client = client_builder(
        HttpTimeouts {
            connect: Some(kraai_test_support::HANG_TIMEOUT),
            read: Some(kraai_test_support::HANG_TIMEOUT),
            request: None,
        },
        reqwest::redirect::Policy::default(),
    )
    .tls_certs_only([])
    .build()
    .unwrap();

    let response = client.get(url).send().await.unwrap();
    tokio::time::pause();
    let mut body = stream_sse_data(response);
    assert_eq!(
        body.next().await.unwrap().unwrap(),
        SseEvent::Data("hello".into())
    );
    let error = body.next().await.unwrap().unwrap_err();

    assert!(matches!(
        error.downcast_ref::<crate::ProviderError>(),
        Some(crate::ProviderError::StreamInterrupted(_))
    ));
    assert!(
        error
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_timeout)
    );
    assert!(body.next().await.is_none());
}
