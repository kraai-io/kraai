use std::io;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, unbounded};
use futures::StreamExt;
use ratatui::crossterm::event::{Event, EventStream};
use tokio::sync::oneshot;

pub(super) type InputEvent = io::Result<Event>;

/// Forwards terminal input to a channel so the UI loop can wait on input and runtime messages
/// together. Dropping crossterm's `EventStream` interrupts its pending read, which lets the
/// reader stop before an external editor takes over the terminal.
pub(super) struct TerminalInput {
    tx: Sender<InputEvent>,
    rx: Receiver<InputEvent>,
    reader: Option<(oneshot::Sender<()>, JoinHandle<()>)>,
}

impl Default for TerminalInput {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self {
            tx,
            rx,
            reader: None,
        }
    }
}

impl TerminalInput {
    pub(super) fn receiver(&self) -> &Receiver<InputEvent> {
        &self.rx
    }

    pub(super) fn start(&mut self) -> io::Result<()> {
        if self.reader.is_some() {
            return Ok(());
        }
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let tx = self.tx.clone();
        let reader = std::thread::Builder::new()
            .name(String::from("kraai-input"))
            .spawn(move || {
                runtime.block_on(async move {
                    let mut events = EventStream::new();
                    loop {
                        let event = tokio::select! {
                            _ = &mut stop_rx => return,
                            event = events.next() => event,
                        };
                        let Some(event) = event else {
                            return;
                        };
                        let failed = event.is_err();
                        if tx.send(event).is_err() || failed {
                            return;
                        }
                    }
                });
            })?;
        self.reader = Some((stop_tx, reader));
        Ok(())
    }

    pub(super) fn stop(&mut self) {
        if let Some((stop, reader)) = self.reader.take() {
            let _ = stop.send(());
            let _ = reader.join();
        }
    }
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        self.stop();
    }
}
