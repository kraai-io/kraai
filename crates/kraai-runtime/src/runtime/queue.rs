use std::collections::{HashMap, HashSet, VecDeque, hash_map::Entry};
use std::sync::{Arc, Mutex};

use tokio::sync::{Notify, mpsc};

use kraai_types::{ModelId, ProviderId, SessionId};

use kraai_persistence::QueuedMessage;

use super::core::RuntimeCore;
use super::streaming::StreamJobKind;
use crate::handle::Command;
use crate::{RuntimeError, RuntimeResult, SubmitMessageOutcome};

/// Coalesce overlapping preparations without blocking the command loop.
#[derive(Default)]
pub(crate) struct SessionPreparations {
    active: Mutex<HashMap<SessionId, Option<Arc<QueueDrains>>>>,
    ready: Notify,
}

pub(crate) struct SessionPreparation {
    preparations: Arc<SessionPreparations>,
    session_id: SessionId,
}

impl SessionPreparations {
    pub(crate) fn try_begin(
        self: &Arc<Self>,
        session_id: &SessionId,
    ) -> Option<SessionPreparation> {
        self.try_begin_with_drain(session_id, None)
    }

    fn try_begin_with_drain(
        self: &Arc<Self>,
        session_id: &SessionId,
        drain: Option<&Arc<QueueDrains>>,
    ) -> Option<SessionPreparation> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match active.entry(session_id.clone()) {
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
            session_id: session_id.clone(),
        })
    }

    pub(crate) async fn begin(self: &Arc<Self>, session_id: &SessionId) -> SessionPreparation {
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

    pub(crate) fn is_active(&self, session_id: &SessionId) -> bool {
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
    sessions: VecDeque<SessionId>,
    scheduled: HashSet<SessionId>,
}

/// Internal wakeups must never wait for space in the channel consumed by the
/// command currently running. Keep one pending wakeup per session, in arrival order.
#[derive(Default)]
pub(crate) struct QueueDrains {
    pending: Mutex<PendingDrains>,
    ready: Notify,
}

impl QueueDrains {
    fn schedule(&self, session_id: &SessionId) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if pending.scheduled.insert(session_id.clone()) {
            pending.sessions.push_back(session_id.clone());
        }
        drop(pending);
        self.ready.notify_one();
    }

    async fn next(&self) -> SessionId {
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
    pub(crate) fn schedule_queue_drain(&self, session_id: &SessionId) {
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
        session_id: SessionId,
        message: kraai_types::MessageContent,
        model_id: ModelId,
        provider_id: ProviderId,
        options: kraai_types::ModelOptionValues,
    ) -> RuntimeResult<SubmitMessageOutcome> {
        let has_pending_messages = self.has_pending_messages(&session_id).await?;
        self.agent_manager
            .read()
            .await
            .validate_model_options(&provider_id, &model_id, &options, true)
            .await
            .map_err(RuntimeError::from_report)?;
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
        let active_script = self.has_active_script_tasks(&session_id).await;
        let has_pending_messages =
            has_pending_messages || self.has_pending_messages(&session_id).await?;
        let agent = self.agent_manager.write().await;
        let preparation =
            if agent.is_turn_active(&session_id) || has_pending_messages || active_script {
                None
            } else {
                self.session_preparations
                    .try_begin_with_drain(&session_id, Some(&self.queue_drains))
            };
        let Some(preparation) = preparation else {
            drop(agent);
            if !self
                .session_store
                .owns_turn(&session_id)
                .await
                .map_err(RuntimeError::from_report)?
            {
                self.session_store
                    .claim_turn(&session_id)
                    .await
                    .map_err(RuntimeError::from_report)?;
                if let Err(error) = self.recover_session_executions(&session_id).await {
                    self.release_turn(&session_id).await;
                    return Err(RuntimeError::from_report(error));
                }
            }
            let position = self
                .session_store
                .enqueue_message(
                    &session_id,
                    &QueuedMessage {
                        content: message,
                        selection: kraai_types::ModelSelection {
                            provider_id,
                            model_id,
                            options,
                        },
                    },
                )
                .await
                .map_err(RuntimeError::from_report)?;
            self.schedule_queue_drain(&session_id);
            return Ok(SubmitMessageOutcome::Queued { position });
        };

        let newly_claimed = !self
            .session_store
            .owns_turn(&session_id)
            .await
            .map_err(RuntimeError::from_report)?;
        if newly_claimed {
            self.session_store
                .claim_turn(&session_id)
                .await
                .map_err(RuntimeError::from_report)?;
        }
        drop(agent);
        if newly_claimed && let Err(error) = self.recover_session_executions(&session_id).await {
            self.release_turn(&session_id).await;
            return Err(RuntimeError::from_report(error));
        }
        let mut agent = self.agent_manager.write().await;
        let stream_request = {
            let result = agent
                .prepare_start_stream(&session_id, message, model_id, provider_id, options)
                .await;
            let providers = agent.cloned_provider_manager();
            drop(agent);
            match result {
                Ok(request) => (providers, request),
                Err(error) => {
                    self.finish_turn(&session_id).await;
                    return Err(RuntimeError::from_report(error));
                }
            }
        };

        let (providers, request) = stream_request;
        let message_id = request.message_id.to_string();

        self.start_stream_job(StreamJobKind::Initial, session_id, providers, request)
            .await;
        drop(preparation);
        Ok(SubmitMessageOutcome::Started { message_id })
    }

    async fn has_pending_messages(&self, session_id: &SessionId) -> RuntimeResult<bool> {
        Ok(self.session_preparations.is_active(session_id)
            || self
                .session_store
                .has_queued_messages(session_id)
                .await
                .map_err(RuntimeError::from_report)?)
    }

    pub(crate) async fn handle_start_queued_messages(&self, session_id: SessionId) {
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
        if is_turn_active || self.has_active_script_tasks(&session_id).await {
            return;
        }

        let queued = match self.session_store.queued_messages(&session_id).await {
            Ok(queued) => queued,
            Err(error) => {
                self.send_session_report_error(&session_id, error);
                return;
            }
        };
        let Some(last) = queued.last() else {
            return;
        };
        let selection = last.message.selection.clone();

        let stream_request = {
            let mut agent = self.agent_manager.write().await;
            let result = agent
                .prepare_queued_stream(&session_id, queued, selection)
                .await;
            if result.is_err() {
                agent.clear_active_turn(&session_id);
                self.event_tx.finish_timer(&session_id);
            }
            let providers = agent.cloned_provider_manager();
            drop(agent);
            if result.is_err() {
                self.release_turn(&session_id).await;
            }
            match result {
                Ok(Some(result)) => (providers, result),
                Ok(None) => return,
                Err(error) => {
                    self.send_session_report_error(&session_id, error);
                    return;
                }
            }
        };

        let (providers, request) = stream_request;
        self.start_stream_job(StreamJobKind::Initial, session_id, providers, request)
            .await;
        drop(preparation);
    }
}
