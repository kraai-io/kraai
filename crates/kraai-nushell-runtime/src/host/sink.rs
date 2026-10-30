use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use nu_protocol::ShellError;
use nu_protocol::shell_error::generic::GenericError;

use crate::host_calls::DescriptorClient;
use crate::rendering::{Diagnostic, Marker, OutputFrame, OutputValue};

const BATCH_BYTES: usize = 256 * 1024;

pub(super) struct OutputSink {
    client: Arc<DescriptorClient>,
    marker: Marker,
    next_id: AtomicU64,
    /// Held while taking and sending queued items so frames keep their order.
    writer: Mutex<()>,
    batch: Mutex<Option<Batch>>,
    queued: Condvar,
}

struct Batch {
    list: u64,
    values: Vec<OutputValue>,
    bytes: usize,
}

impl OutputSink {
    pub(super) fn new(client: Arc<DescriptorClient>, marker: Marker) -> std::io::Result<Arc<Self>> {
        let sink = Arc::new(Self {
            client,
            marker,
            next_id: AtomicU64::new(1),
            writer: Mutex::new(()),
            batch: Mutex::new(None),
            queued: Condvar::new(),
        });
        let flusher = Arc::clone(&sink);
        std::thread::Builder::new()
            .name(String::from("kraai-output"))
            .spawn(move || flusher.flush_queued())?;
        Ok(sink)
    }

    pub(super) fn value(
        &self,
        value: OutputValue,
        stderr: bool,
        newline: bool,
    ) -> Result<(), Box<ShellError>> {
        let id = self.id();
        self.send(OutputFrame::Value { id, value })?;
        self.place(id, stderr, newline)
    }

    pub(super) fn open_list(&self) -> u64 {
        self.id()
    }

    pub(super) fn item(&self, list: u64, value: OutputValue) -> Result<(), Box<ShellError>> {
        let bytes = value.size_hint();
        loop {
            let mut batch = self.lock()?;
            match batch.as_mut() {
                Some(queued) if queued.list != list || queued.bytes + bytes > BATCH_BYTES => {
                    drop(batch);
                    self.flush()?;
                }
                Some(queued) => {
                    queued.values.push(value);
                    queued.bytes += bytes;
                    return Ok(());
                }
                None => {
                    *batch = Some(Batch {
                        list,
                        values: vec![value],
                        bytes,
                    });
                    drop(batch);
                    self.queued.notify_one();
                    return Ok(());
                }
            }
        }
    }

    pub(super) fn close_list(
        &self,
        list: u64,
        stderr: bool,
        newline: bool,
    ) -> Result<(), Box<ShellError>> {
        let id = self.id();
        self.send(OutputFrame::ListEnd { id, list })?;
        self.place(id, stderr, newline)
    }

    pub(super) fn diagnostic(&self, diagnostic: Diagnostic) {
        let id = self.id();
        let fallback = diagnostic.render();
        let sent = self
            .send(OutputFrame::Diagnostic { id, diagnostic })
            .and_then(|()| self.place(id, true, true));
        if sent.is_err() {
            let _ = writeln!(std::io::stderr().lock(), "{fallback}");
        }
    }

    /// Sends queued items before output that bypasses the frame channel, so the parent has every
    /// item produced before that output even if the host is killed right after writing it.
    pub(super) fn flush(&self) -> Result<(), Box<ShellError>> {
        self.send_batch(&self.writer()?)
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn send(&self, frame: OutputFrame) -> Result<(), Box<ShellError>> {
        let writer = self.writer()?;
        self.send_batch(&writer)?;
        self.client.send_output(frame).map_err(failure)
    }

    fn send_batch(&self, _writer: &MutexGuard<'_, ()>) -> Result<(), Box<ShellError>> {
        let Some(Batch { list, values, .. }) = self.lock()?.take() else {
            return Ok(());
        };
        self.client
            .send_output(OutputFrame::Items { list, values })
            .map_err(failure)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Option<Batch>>, Box<ShellError>> {
        self.batch.lock().map_err(failure)
    }

    fn writer(&self) -> Result<MutexGuard<'_, ()>, Box<ShellError>> {
        self.writer.lock().map_err(failure)
    }

    fn flush_queued(&self) {
        loop {
            let Ok(mut batch) = self.batch.lock() else {
                return;
            };
            while batch.is_none() {
                batch = match self.queued.wait(batch) {
                    Ok(batch) => batch,
                    Err(_poisoned) => return,
                };
            }
            drop(batch);
            let Ok(writer) = self.writer() else {
                return;
            };
            let _ = self.send_batch(&writer);
        }
    }

    fn place(&self, id: u64, stderr: bool, newline: bool) -> Result<(), Box<ShellError>> {
        let mut bytes = self.marker.encode(id);
        if newline {
            bytes.push(b'\n');
        }
        let written = if stderr {
            write_flushed(std::io::stderr().lock(), &bytes)
        } else {
            write_flushed(std::io::stdout().lock(), &bytes)
        };
        written.map_err(failure)
    }
}

fn failure(error: impl ToString) -> Box<ShellError> {
    Box::new(ShellError::Generic(GenericError::new_internal(
        "Kraai output channel failed",
        error.to_string(),
    )))
}

fn write_flushed(mut dest: impl Write, bytes: &[u8]) -> std::io::Result<()> {
    dest.write_all(bytes)?;
    dest.flush()
}
