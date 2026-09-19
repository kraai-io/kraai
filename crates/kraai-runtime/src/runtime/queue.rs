use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::{Notify, mpsc};

use kraai_types::{ModelId, ProviderId};

use super::core::{QueuedMessage, RuntimeCore};
use super::streaming::StreamJobKind;
use crate::handle::Command;
use crate::{RuntimeError, RuntimeResult, SubmitMessageOutcome};

/// Coalesce overlapping preparations without blocking the command loop.
#[derive(Default)]
pub(crate) struct SessionPreparations {
    active: Mutex<HashSet<String>>,
}

pub(crate) struct SessionPreparation {
    preparations: Arc<SessionPreparations>,
    session_id: String,
}

impl SessionPreparations {
    pub(crate) fn try_begin(self: &Arc<Self>, session_id: &str) -> Option<SessionPreparation> {
        let inserted = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(session_id.to_string());
        inserted.then(|| SessionPreparation {
            preparations: self.clone(),
            session_id: session_id.to_string(),
        })
    }

    pub(crate) fn is_active(&self, session_id: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains(session_id)
    }
}

impl Drop for SessionPreparation {
    fn drop(&mut self) {
        self.preparations
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.session_id);
    }
}

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

impl RuntimeCore {
    pub(crate) async fn handle_send_message(
        &self,
        session_id: String,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> RuntimeResult<SubmitMessageOutcome> {
        let has_pending_messages = {
            let queued = self.queued_messages.lock().await;
            queued
                .get(&session_id)
                .is_some_and(|queue| !queue.is_empty())
                || self.session_preparations.is_active(&session_id)
        };
        let mut agent = self.agent_manager.write().await;
        if agent.is_turn_active(&session_id)
            || has_pending_messages
            || self.session_preparations.is_active(&session_id)
        {
            drop(agent);
            let position = self
                .enqueue_message(
                    &session_id,
                    QueuedMessage {
                        message,
                        model_id,
                        provider_id,
                    },
                )
                .await;
            self.schedule_queue_drain(&session_id);
            return Ok(SubmitMessageOutcome::Queued { position });
        }

        let stream_request = {
            let result = agent
                .prepare_start_stream(&session_id, message, model_id, provider_id)
                .await;
            let providers = agent.cloned_provider_manager();
            drop(agent);
            result
                .map(|result| (providers, result))
                .map_err(RuntimeError::from_report)?
        };

        let (providers, request) = stream_request;
        let message_id = request.message_id.to_string();

        self.start_stream_job(StreamJobKind::Initial, session_id, providers, request)
            .await;
        Ok(SubmitMessageOutcome::Started { message_id })
    }

    async fn enqueue_message(&self, session_id: &str, queued_message: QueuedMessage) -> usize {
        let mut queued = self.queued_messages.lock().await;
        let queue = queued.entry(session_id.to_string()).or_default();
        queue.push_back(queued_message);
        let position = queue.len();
        drop(queued);
        position
    }

    pub(crate) async fn handle_start_queued_messages(&self, session_id: String) {
        let Some(preparation) = self.session_preparations.try_begin(&session_id) else {
            return;
        };
        let is_turn_active = {
            let agent = self.agent_manager.read().await;
            agent.is_turn_active(&session_id)
        };
        if is_turn_active {
            return;
        }

        let messages = self.take_queued_messages(&session_id).await;
        let Some(last_message) = messages.last() else {
            return;
        };
        let model_id = last_message.model_id.clone();
        let provider_id = last_message.provider_id.clone();
        let contents = messages
            .iter()
            .map(|message| message.message.clone())
            .collect();

        let stream_request = {
            let mut agent = self.agent_manager.write().await;
            let result = agent
                .prepare_intercepted_stream(&session_id, contents, model_id, provider_id)
                .await;
            if result.is_err() {
                agent.clear_active_turn(&session_id);
                self.event_tx.finish_timer(&session_id);
            }
            let providers = agent.cloned_provider_manager();
            drop(agent);
            match result {
                Ok(Some(result)) => Some((providers, result)),
                Ok(None) => {
                    self.restore_queued_messages(&session_id, messages).await;
                    return;
                }
                Err(error) => {
                    self.restore_queued_messages(&session_id, messages).await;
                    self.send_session_report_error(&session_id, error);
                    None
                }
            }
        };

        let Some((providers, request)) = stream_request else {
            return;
        };

        drop(preparation);
        self.start_stream_job(StreamJobKind::Initial, session_id, providers, request)
            .await;
    }

    pub(crate) async fn take_queued_messages(&self, session_id: &str) -> Vec<QueuedMessage> {
        self.queued_messages
            .lock()
            .await
            .remove(session_id)
            .map(VecDeque::into_iter)
            .map(Iterator::collect)
            .unwrap_or_default()
    }

    pub(crate) async fn restore_queued_messages(
        &self,
        session_id: &str,
        messages: Vec<QueuedMessage>,
    ) {
        let mut queued = self.queued_messages.lock().await;
        let queue = queued.entry(session_id.to_string()).or_default();
        for message in messages.into_iter().rev() {
            queue.push_front(message);
        }
        drop(queued);
    }
}
