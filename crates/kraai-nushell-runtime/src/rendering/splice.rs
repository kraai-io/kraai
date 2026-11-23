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
mod tests;
