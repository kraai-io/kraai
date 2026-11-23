use kraai_sandbox::{OutputEvent, OutputStream};

use super::{List, Marker, OutputFrame, OutputValue, render};

#[test]
fn long_lists_stop_keeping_rows_once_they_fill_the_capture_but_keep_counting() {
    let mut list = List::default();
    let rows = kraai_sandbox::MAX_CAPTURE_BYTES;
    for _ in 0..rows {
        list.push(OutputValue::Text("0123456789".into()));
    }
    assert!(list.items.len() < rows / 10);
    let rendered = list.render();
    assert!(rendered.starts_with(&format!("[{rows}]:\n")));
    assert!(rendered.len() > kraai_sandbox::MAX_CAPTURE_BYTES);
}

fn event(stream: OutputStream, bytes: &[u8]) -> OutputEvent {
    OutputEvent {
        stream,
        bytes: bytes.to_vec(),
    }
}

#[tokio::test]
async fn markers_are_replaced_by_frames_that_arrive_later_even_when_split() {
    let marker = Marker::random();
    let (frames_tx, frames) = tokio::sync::mpsc::channel(8);
    let (raw_tx, raw) = tokio::sync::mpsc::channel(8);
    let (events_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(render(marker.clone(), frames, raw, Some(events_tx)));

    let value = marker.encode(1);
    let (head, tail) = value.split_at(5);
    let mut first = b"before\n".to_vec();
    first.extend_from_slice(head);
    raw_tx.send(event(OutputStream::Stdout, &first)).await.ok();
    let mut second = tail.to_vec();
    second.extend_from_slice(b"\n");
    second.extend(marker.encode(3));
    second.extend_from_slice(b"\n\x1eplain\n");
    raw_tx.send(event(OutputStream::Stdout, &second)).await.ok();
    raw_tx
        .send(event(OutputStream::Stderr, &marker.encode(9)))
        .await
        .ok();
    tokio::task::yield_now().await;
    for frame in [
        OutputFrame::Value {
            id: 1,
            value: OutputValue::Int(4),
        },
        OutputFrame::Items {
            list: 2,
            values: vec![OutputValue::Int(5)],
        },
        OutputFrame::Items {
            list: 2,
            values: vec![OutputValue::Int(6)],
        },
        OutputFrame::ListEnd { id: 3, list: 2 },
        OutputFrame::Items {
            list: 4,
            values: vec![OutputValue::Text("cut off".into())],
        },
    ] {
        frames_tx.send(frame).await.ok();
    }
    drop(raw_tx);
    drop(frames_tx);

    let (stdout, stderr) = task.await.unwrap();
    assert_eq!(
        String::from_utf8_lossy(&stdout),
        "before\n4\n[2]: 5,6\n\x1eplain\n[1]: cut off\n"
    );
    assert!(stderr.is_empty());
    let mut streamed = Vec::new();
    while let Some(event) = events.recv().await {
        if event.stream == OutputStream::Stdout {
            streamed.extend(event.bytes);
        }
    }
    assert_eq!(streamed, stdout);
}
