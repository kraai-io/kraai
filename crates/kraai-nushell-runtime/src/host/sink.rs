use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use nu_protocol::ShellError;
use nu_protocol::shell_error::generic::GenericError;

use crate::host_calls::DescriptorClient;
use crate::rendering::{Diagnostic, Marker, OutputFrame, OutputValue};

const BATCH_BYTES: usize = 256 * 1024;
const BATCH_DELAY: Duration = Duration::from_millis(5);

pub(super) struct OutputSink {
    client: Arc<DescriptorClient>,
    marker: Marker,
    next_id: AtomicU64,
    batch: Mutex<Option<Batch>>,
    queued: Condvar,
}

struct Batch {
    list: u64,
    values: Vec<OutputValue>,
    bytes: usize,
    started: Instant,
}

impl OutputSink {
    pub(super) fn new(client: Arc<DescriptorClient>, marker: Marker) -> std::io::Result<Arc<Self>> {
        let sink = Arc::new(Self {
            client,
            marker,
            next_id: AtomicU64::new(1),
            batch: Mutex::new(None),
            queued: Condvar::new(),
        });
        let flusher = Arc::clone(&sink);
        std::thread::Builder::new()
            .name(String::from("kraai-output"))
            .spawn(move || flusher.flush_idle())?;
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
        let mut batch = self.lock()?;
        if batch
            .as_ref()
            .is_some_and(|batch| batch.list != list || batch.bytes + bytes > BATCH_BYTES)
        {
            self.flush(&mut batch)?;
        }
        if let Some(batch) = batch.as_mut() {
            batch.values.push(value);
            batch.bytes += bytes;
        } else {
            *batch = Some(Batch {
                list,
                values: vec![value],
                bytes,
                started: Instant::now(),
            });
            self.queued.notify_one();
        }
        drop(batch);
        Ok(())
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

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn send(&self, frame: OutputFrame) -> Result<(), Box<ShellError>> {
        let mut batch = self.lock()?;
        self.flush(&mut batch)?;
        self.client.send_output(frame).map_err(failure)
    }

    fn flush(&self, batch: &mut Option<Batch>) -> Result<(), Box<ShellError>> {
        let Some(Batch { list, values, .. }) = batch.take() else {
            return Ok(());
        };
        self.client
            .send_output(OutputFrame::Items { list, values })
            .map_err(failure)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Option<Batch>>, Box<ShellError>> {
        self.batch.lock().map_err(failure)
    }

    fn flush_idle(&self) {
        let Ok(mut batch) = self.batch.lock() else {
            return;
        };
        loop {
            let remaining = batch.as_ref().map(|batch| {
                (batch.started + BATCH_DELAY).saturating_duration_since(Instant::now())
            });
            batch = match remaining {
                None => match self.queued.wait(batch) {
                    Ok(batch) => batch,
                    Err(_poisoned) => return,
                },
                Some(Duration::ZERO) => {
                    let _ = self.flush(&mut batch);
                    continue;
                }
                Some(remaining) => match self.queued.wait_timeout(batch, remaining) {
                    Ok((batch, _)) => batch,
                    Err(_poisoned) => return,
                },
            };
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
