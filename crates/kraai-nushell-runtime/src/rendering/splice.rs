use std::collections::{BTreeMap, HashMap};

use kraai_sandbox::{MAX_CAPTURE_BYTES, OutputCapture, OutputEvent, OutputStream};
use tokio::sync::mpsc::{Receiver, UnboundedSender};

use super::marker::Scan;
use super::{Marker, OutputFrame, OutputValue, toon};

const FRAME_GRACE: std::time::Duration = std::time::Duration::from_millis(100);

pub(crate) async fn render(
    marker: Marker,
    frames: Receiver<OutputFrame>,
    mut raw: Receiver<OutputEvent>,
    events: Option<UnboundedSender<OutputEvent>>,
) -> (Vec<u8>, Vec<u8>) {
    let mut splice = Splice {
        marker,
        frames,
        frames_closed: false,
        ready: HashMap::new(),
        lists: BTreeMap::new(),
        stdout: Channel::default(),
        stderr: Channel::default(),
        events,
    };
    loop {
        tokio::select! {
            biased;
            frame = splice.frames.recv(), if !splice.frames_closed => match frame {
                Some(frame) => splice.apply(frame),
                None => splice.frames_closed = true,
            },
            event = raw.recv() => match event {
                Some(event) => splice.push(event.stream, &event.bytes).await,
                None => break,
            },
        }
    }
    splice.finish().await
}

#[derive(Default)]
struct Channel {
    pending: Vec<u8>,
    capture: OutputCapture,
}

#[derive(Default)]
struct List {
    items: Vec<OutputValue>,
    len: usize,
    kept_bytes: usize,
}

impl List {
    fn push(&mut self, value: OutputValue) {
        self.len += 1;
        if self.kept_bytes <= MAX_CAPTURE_BYTES {
            self.kept_bytes += toon::min_len(&value) + 1;
            self.items.push(value);
        }
    }

    fn render(&self) -> String {
        toon::list(&self.items, self.len)
    }
}

struct Splice {
    marker: Marker,
    frames: Receiver<OutputFrame>,
    frames_closed: bool,
    ready: HashMap<u64, String>,
    lists: BTreeMap<u64, List>,
    stdout: Channel,
    stderr: Channel,
    events: Option<UnboundedSender<OutputEvent>>,
}

impl Splice {
    fn apply(&mut self, frame: OutputFrame) {
        match frame {
            OutputFrame::Value { id, value } => {
                self.ready.insert(id, toon::render(&value));
            }
            OutputFrame::Items { list, values } => {
                let list = self.lists.entry(list).or_default();
                for value in values {
                    list.push(value);
                }
            }
            OutputFrame::ListEnd { id, list } => {
                let list = self.lists.remove(&list).unwrap_or_default();
                self.ready.insert(id, list.render());
            }
            OutputFrame::Diagnostic { id, diagnostic } => {
                self.ready.insert(id, diagnostic.render());
            }
        }
    }

    async fn push(&mut self, stream: OutputStream, bytes: &[u8]) {
        let mut pending = std::mem::take(&mut self.channel(stream).pending);
        pending.extend_from_slice(bytes);
        let mut position = 0;
        while let Some(rest) = pending.get(position..).filter(|rest| !rest.is_empty()) {
            match self.marker.scan(rest) {
                Scan::Text(length) => {
                    let text = rest.get(..length).unwrap_or(rest).to_vec();
                    self.emit(stream, &text);
                    position += length;
                }
                Scan::Marker { length, id } => {
                    if let Some(text) = self.take(id).await {
                        self.emit(stream, text.as_bytes());
                    }
                    position += length;
                }
                Scan::Incomplete => break,
            }
        }
        pending.drain(..position);
        self.channel(stream).pending = pending;
    }

    async fn take(&mut self, id: u64) -> Option<String> {
        loop {
            if let Some(text) = self.ready.remove(&id) {
                return Some(text);
            }
            if self.frames_closed {
                return None;
            }
            match self.frames.recv().await {
                Some(frame) => self.apply(frame),
                None => self.frames_closed = true,
            }
        }
    }

    fn channel(&mut self, stream: OutputStream) -> &mut Channel {
        match stream {
            OutputStream::Stdout => &mut self.stdout,
            OutputStream::Stderr => &mut self.stderr,
        }
    }

    fn emit(&mut self, stream: OutputStream, bytes: &[u8]) {
        let emitted = self.channel(stream).capture.push(bytes).to_vec();
        self.send(stream, emitted);
    }

    fn send(&self, stream: OutputStream, bytes: Vec<u8>) {
        if !bytes.is_empty()
            && let Some(events) = &self.events
        {
            let _ = events.send(OutputEvent { stream, bytes });
        }
    }

    async fn finish(mut self) -> (Vec<u8>, Vec<u8>) {
        let deadline = tokio::time::Instant::now() + FRAME_GRACE;
        while !self.frames_closed {
            match tokio::time::timeout_at(deadline, self.frames.recv()).await {
                Ok(Some(frame)) => self.apply(frame),
                Ok(None) => self.frames_closed = true,
                Err(_elapsed) => break,
            }
        }
        for stream in [OutputStream::Stdout, OutputStream::Stderr] {
            let pending = std::mem::take(&mut self.channel(stream).pending);
            self.emit(stream, &pending);
        }
        for (_, list) in std::mem::take(&mut self.lists) {
            let mut text = list.render();
            text.push('\n');
            self.emit(OutputStream::Stdout, text.as_bytes());
        }
        let mut captured = [Vec::new(), Vec::new()];
        for (stream, captured) in [OutputStream::Stdout, OutputStream::Stderr]
            .into_iter()
            .zip(&mut captured)
        {
            let channel = std::mem::take(self.channel(stream));
            let mut capture = channel.capture;
            let rest = capture.finish().to_vec();
            self.send(stream, rest);
            *captured = capture.into_bytes();
        }
        let [stdout, stderr] = captured;
        (stdout, stderr)
    }
}

#[cfg(test)]
mod tests {
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
    #[expect(clippy::panic_in_result_fn, reason = "test assertions report failures")]
    async fn markers_are_replaced_by_frames_that_arrive_later_even_when_split()
    -> Result<(), Box<dyn std::error::Error>> {
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

        let (stdout, stderr) = task.await?;
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
        Ok(())
    }
}
