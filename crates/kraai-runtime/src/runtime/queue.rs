use std::collections::{HashMap, HashSet, VecDeque, hash_map::Entry};
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
    active: Mutex<HashMap<String, Option<Arc<QueueDrains>>>>,
    ready: Notify,
}

pub(crate) struct SessionPreparation {
    preparations: Arc<SessionPreparations>,
    session_id: String,
}

impl SessionPreparations {
    pub(crate) fn try_begin(self: &Arc<Self>, session_id: &str) -> Option<SessionPreparation> {
        self.try_begin_with_drain(session_id, None)
    }

    fn try_begin_with_drain(
        self: &Arc<Self>,
        session_id: &str,
        drain: Option<&Arc<QueueDrains>>,
    ) -> Option<SessionPreparation> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match active.entry(session_id.to_string()) {
            Entry::Vacant(entry) => {
                entry.insert(None);
            }
            Entry::Occupied(mut entry) => {
                if let Some(drain) = drain {
                    entry.insert(Some(drain.clone()));
                }
                return None;
            }
        }
        drop(active);
        Some(SessionPreparation {
            preparations: self.clone(),
            session_id: session_id.to_string(),
        })
    }

    pub(crate) async fn begin(self: &Arc<Self>, session_id: &str) -> SessionPreparation {
        loop {
            let ready = self.ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if let Some(preparation) = self.try_begin(session_id) {
                return preparation;
            }
            ready.await;
        }
    }

    pub(crate) fn is_active(&self, session_id: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(session_id)
    }
}

impl Drop for SessionPreparation {
    fn drop(&mut self) {
        let drain = self
            .preparations
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.session_id);
        self.preparations.ready.notify_waiters();
        if let Some(Some(drain)) = drain {
            drain.schedule(&self.session_id);
        }
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
        message: kraai_types::MessageContent,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> RuntimeResult<SubmitMessageOutcome> {
        if message.images().count() > kraai_types::image::MAX_IMAGE_ATTACHMENTS {
            return Err(RuntimeError::invalid_argument("Too many image attachments"));
        }
        if message
            .images()
            .try_fold(0u64, |total, image| total.checked_add(image.byte_length))
            .is_none_or(|total| total > kraai_types::image::MAX_REQUEST_IMAGE_BYTES)
        {
            return Err(RuntimeError::invalid_argument(
                "Image attachments exceed the request byte limit",
            ));
        }
        for image in message.images() {
            self.image_store
                .read(image)
                .await
                .map_err(RuntimeError::internal)?;
        }
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
        let Some(preparation) = self
            .session_preparations
            .try_begin_with_drain(&session_id, Some(&self.queue_drains))
        else {
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
        let messages = self.queued_messages.lock().await.remove(session_id);
        messages.map(Vec::from).unwrap_or_default()
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
