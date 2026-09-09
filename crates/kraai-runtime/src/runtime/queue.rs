use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

use tokio::sync::{Notify, mpsc};

use super::core::RuntimeCore;
use crate::handle::Command;

#[derive(Default)]
struct PendingDrains {
    sessions: VecDeque<String>,
    scheduled: HashSet<String>,
}

/// Internal wakeups must never wait for space in the channel consumed by the
/// command currently running. Keep one pending wakeup per session, in arrival order.
#[derive(Default)]
pub(crate) struct QueueDrains {
    pending: Mutex<PendingDrains>,
    ready: Notify,
}

impl QueueDrains {
    fn schedule(&self, session_id: &str) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if pending.scheduled.insert(session_id.to_string()) {
            pending.sessions.push_back(session_id.to_string());
        }
        drop(pending);
        self.ready.notify_one();
    }

    async fn next(&self) -> String {
        loop {
            self.ready.notified().await;
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let session = pending.sessions.pop_front();
            if let Some(session) = &session {
                pending.scheduled.remove(session);
            }
            let more = !pending.sessions.is_empty();
            drop(pending);
            if more {
                self.ready.notify_one();
            }
            if let Some(session) = session {
                return session;
            }
        }
    }
}

impl RuntimeCore {
    pub(crate) fn schedule_queue_drain(&self, session_id: &str) {
        self.queue_drains.schedule(session_id);
    }

    pub(crate) async fn next_command(
        &self,
        commands: &mut mpsc::Receiver<Command>,
    ) -> Option<Command> {
        tokio::select! {
            command = commands.recv() => command,
            session_id = self.queue_drains.next() => Some(Command::StartQueuedMessages { session_id }),
        }
    }
}
