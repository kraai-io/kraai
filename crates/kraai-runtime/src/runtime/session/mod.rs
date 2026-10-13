//! One actor per session owns that session's turn state and applies every change to it in
//! order. Long work runs in child tasks that report back to the actor.

mod actor;
mod cancel;
mod scripts;
mod snapshot;
mod stream;
#[cfg(test)]
mod test_support;
mod turn;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use kraai_types::SessionId;
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio_util::task::TaskTracker;

#[cfg(test)]
pub(in crate::runtime) use actor::SessionActor;
pub(crate) use actor::{ActivityKind, SessionMessage};
pub(super) use stream::StreamEvent;
#[cfg(test)]
pub(in crate::runtime) use stream::apply_stream_event;

use super::core::RuntimeCore;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Actors {
    senders: HashMap<SessionId, mpsc::UnboundedSender<SessionMessage>>,
    /// Set once the runtime stops; no actor may start after that.
    closed: bool,
}

#[derive(Default)]
pub(crate) struct SessionActors {
    actors: Mutex<Actors>,
    activity: Mutex<HashMap<SessionId, ActivityKind>>,
    barriers: Mutex<HashMap<SessionId, Arc<RwLock<()>>>>,
    tasks: TaskTracker,
}

impl SessionActors {
    /// Orders commits made outside the actor against snapshots of the same session.
    pub(crate) fn barrier(&self, session_id: &SessionId) -> Arc<RwLock<()>> {
        lock(&self.barriers)
            .entry(session_id.clone())
            .or_default()
            .clone()
    }

    pub(crate) fn activity(&self, session_id: &SessionId) -> ActivityKind {
        lock(&self.activity)
            .get(session_id)
            .copied()
            .unwrap_or(ActivityKind::Idle)
    }

    fn publish_activity(&self, session_id: &SessionId, activity: ActivityKind) {
        let mut published = lock(&self.activity);
        if activity == ActivityKind::Idle {
            published.remove(session_id);
        } else {
            published.insert(session_id.clone(), activity);
        }
    }

    pub(crate) fn send(
        &self,
        runtime: &RuntimeCore,
        session_id: &SessionId,
        message: SessionMessage,
    ) {
        let mut actors = lock(&self.actors);
        if actors.closed {
            message.reject();
            return;
        }
        let message = match actors.senders.get(session_id) {
            Some(sender) => match sender.send(message) {
                Ok(()) => return,
                Err(mpsc::error::SendError(message)) => message,
            },
            None => message,
        };
        let sender = actor::spawn(runtime.clone(), session_id.clone(), &self.tasks);
        if let Err(mpsc::error::SendError(message)) = sender.send(message) {
            message.reject();
            return;
        }
        actors.senders.insert(session_id.clone(), sender);
    }

    fn remove(&self, session_id: &SessionId, sender: &mpsc::UnboundedSender<SessionMessage>) {
        let mut actors = lock(&self.actors);
        if actors
            .senders
            .get(session_id)
            .is_some_and(|current| current.same_channel(sender))
        {
            actors.senders.remove(session_id);
        }
        drop(actors);
        lock(&self.activity).remove(session_id);
    }

    /// Stops every actor's work and waits until all actors have exited.
    pub(crate) async fn stop(&self) {
        let actors: Vec<_> = {
            let mut actors = lock(&self.actors);
            actors.closed = true;
            actors.senders.drain().map(|(_, sender)| sender).collect()
        };
        let mut stopped = Vec::with_capacity(actors.len());
        for sender in actors {
            let (response, done) = oneshot::channel();
            if sender.send(SessionMessage::Stop { response }).is_ok() {
                stopped.push(done);
            }
        }
        for done in stopped {
            let _ = done.await;
        }
        self.tasks.close();
        self.tasks.wait().await;
    }
}
