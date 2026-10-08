use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use color_eyre::eyre::Result;
use futures::FutureExt;
use kraai_agent::PendingStreamRequest;
use kraai_provider_core::ProviderManager;
use kraai_types::{MessageId, ModelId, ProviderId};
use tokio::sync::Notify;

use super::core::{ActiveStream, RuntimeCore, emit_event};
use super::queue::SessionPreparation;
use super::stream_driver::{CompletedStreamOutput, StreamDriveResult};
use crate::api::Event;
use crate::{ContinueSessionOutcome, RuntimeError, RuntimeResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamJobKind {
    Initial,
    Continuation,
}

impl StreamJobKind {
    fn is_continuation(self) -> bool {
        matches!(self, Self::Continuation)
    }
}

impl RuntimeCore {
    pub(crate) async fn start_continuation(
        &self,
        session_id: String,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> RuntimeResult<ContinueSessionOutcome> {
        if self.is_stopping() {
            return Ok(ContinueSessionOutcome::NothingToContinue);
        }
        let Some(preparation) = self.session_preparations.try_begin(&session_id) else {
            return Ok(ContinueSessionOutcome::NothingToContinue);
        };
        self.continue_prepared_session(session_id, preparation, Some((model_id, provider_id)))
            .await
    }

    pub(crate) async fn start_continuation_when_ready(
        &self,
        session_id: String,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> RuntimeResult<ContinueSessionOutcome> {
        if self.is_stopping() {
            return Ok(ContinueSessionOutcome::NothingToContinue);
        }
        let preparation = self.session_preparations.begin(&session_id).await;
        self.continue_prepared_session(session_id, preparation, Some((model_id, provider_id)))
            .await
    }

    async fn continue_prepared_session(
        &self,
        session_id: String,
        preparation: SessionPreparation,
        selection: Option<(ModelId, ProviderId)>,
    ) -> RuntimeResult<ContinueSessionOutcome> {
        if self.is_stopping() {
            return Ok(ContinueSessionOutcome::NothingToContinue);
        }
        if self
            .pending_script_approvals
            .lock()
            .await
            .contains_key(&session_id)
            || self.has_active_script_tasks(&session_id).await
        {
            return Ok(ContinueSessionOutcome::NothingToContinue);
        }
        let mut retried = false;
        loop {
            let queued_messages = self.take_queued_messages(&session_id).await;
            let intercepted = !queued_messages.is_empty();
            let streaming = {
                let agent = self.agent_manager.read().await;
                agent.session_has_active_stream(&session_id).await
            };
            if streaming {
                self.restore_queued_messages(&session_id, queued_messages)
                    .await;
                return Ok(ContinueSessionOutcome::NothingToContinue);
            }
            let ownership = async {
                if !self.session_store.owns_turn(&session_id).await? {
                    self.session_store.claim_turn(&session_id).await?;
                    if let Err(error) = self.recover_session_executions(&session_id).await {
                        self.release_turn(&session_id).await;
                        return Err(error);
                    }
                }
                Ok(())
            }
            .await;
            if let Err(error) = ownership {
                self.restore_queued_messages(&session_id, queued_messages)
                    .await;
                return Err(RuntimeError::from_report(error));
            }

            let continuation = {
                let mut agent = self.agent_manager.write().await;
                let selected_model = selection.clone().or_else(|| {
                    queued_messages
                        .last()
                        .map(|message| (message.model_id.clone(), message.provider_id.clone()))
                });
                let result = if let Some((model_id, provider_id)) = selected_model {
                    agent
                        .prepare_messages_stream(
                            &session_id,
                            queued_messages
                                .iter()
                                .map(|message| message.message.clone())
                                .collect(),
                            model_id,
                            provider_id,
                        )
                        .await
                } else {
                    agent.prepare_continuation_stream(&session_id).await
                };
                match result {
                    Ok(result) => {
                        Ok(result.map(|request| (agent.cloned_provider_manager(), request)))
                    }
                    Err(error) => Err(error),
                }
            };

            match continuation {
                Ok(Some((providers, request))) => {
                    self.start_stream_job(
                        StreamJobKind::Continuation,
                        session_id,
                        providers,
                        request,
                    )
                    .await;
                    drop(preparation);
                    return Ok(ContinueSessionOutcome::Started);
                }
                Ok(None) => {
                    self.restore_queued_messages(&session_id, queued_messages)
                        .await;
                    self.finish_turn(&session_id).await;
                    return Ok(ContinueSessionOutcome::NothingToContinue);
                }
                Err(error) => {
                    self.restore_queued_messages(&session_id, queued_messages)
                        .await;
                    if intercepted && !retried {
                        retried = true;
                        continue;
                    }
                    self.finish_turn(&session_id).await;
                    if !intercepted {
                        self.schedule_queue_drain(&session_id);
                    }
                    emit_event(
                        &self.event_tx,
                        Event::HistoryUpdated {
                            session_id: session_id.clone(),
                        },
                    );
                    return Err(RuntimeError::from_report(error));
                }
            }
        }
    }

    pub(crate) fn spawn_continuation(&self, session_id: String, expected_tip: MessageId) {
        if self.is_stopping() {
            return;
        }
        let runtime = self.clone();
        tokio::spawn(async move {
            let _state_guard = runtime.session_state_barrier.read().await;
            let preparation = runtime.session_preparations.begin(&session_id).await;
            let result: RuntimeResult<()> = async {
                let agent = runtime.agent_manager.read().await;
                if !agent.is_turn_active(&session_id)
                    || agent
                        .get_tip(&session_id)
                        .await
                        .map_err(RuntimeError::from_report)?
                        .as_ref()
                        != Some(&expected_tip)
                {
                    return Ok(());
                }
                drop(agent);
                runtime
                    .continue_prepared_session(session_id.clone(), preparation, None)
                    .await?;
                Ok(())
            }
            .await;
            if let Err(error) = result {
                emit_event(
                    &runtime.event_tx,
                    Event::ContinuationFailed {
                        session_id,
                        error: error.to_string(),
                    },
                );
            }
        });
    }

    // Callers hold session_state_barrier from request preparation through this method.
    pub(crate) async fn start_stream_job(
        &self,
        kind: StreamJobKind,
        session_id: String,
        providers: ProviderManager,
        mut request: PendingStreamRequest,
    ) {
        let context_notifications = std::mem::take(&mut request.context_notifications);
        let task_runtime = self.clone();
        let start_gate = Arc::new(Notify::new());
        let request_session_id = session_id.clone();
        let request_message_id = request.message_id.clone();
        let active_message_id = request_message_id.clone();
        let terminal_message_id = request_message_id.clone();
        let auxiliary_usage_task = self.stream_tasks.auxiliary_token(&session_id);
        let task = self.stream_tasks.spawn(&session_id, {
            let start_gate = start_gate.clone();
            async move {
                start_gate.notified().await;
                let result = AssertUnwindSafe(RuntimeCore::drive_stream(
                    request_session_id.clone(),
                    request,
                    providers,
                    task_runtime.agent_manager.clone(),
                    task_runtime.event_tx.clone(),
                    task_runtime.session_state_barrier.clone(),
                    auxiliary_usage_task,
                ))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| StreamDriveResult::FailedDuringStream {
                    error: String::from("provider stream task panicked"),
                });

                task_runtime
                    .stream_tasks
                    .wait_auxiliary_session(&request_session_id)
                    .await;
                let _state_guard = task_runtime.session_state_barrier.read().await;
                let _preparation = task_runtime
                    .session_preparations
                    .begin(&request_session_id)
                    .await;
                let stream_was_active = task_runtime
                    .clear_active_stream(&request_session_id, &active_message_id)
                    .await;
                if !stream_was_active {
                    return;
                }

                task_runtime
                    .handle_stream_terminal_state(
                        kind,
                        request_session_id,
                        terminal_message_id,
                        result,
                    )
                    .await;
            }
        });

        #[cfg(test)]
        self.stream_tasks.before_publication().await;

        let previous = self.active_streams.lock().await.insert(
            session_id.clone(),
            ActiveStream {
                message_id: request_message_id.clone(),
                abort_handle: task.abort_handle(),
            },
        );
        if let Some(previous) = previous {
            let agent = self.agent_manager.write().await;
            previous.abort_handle.abort();
            drop(agent);
        }
        if !context_notifications.is_empty() {
            emit_event(
                &self.event_tx,
                Event::ContextStateChanged {
                    session_id: session_id.clone(),
                    notifications: context_notifications,
                },
            );
        }
        emit_event(
            &self.event_tx,
            Event::StreamStart {
                session_id,
                message_id: request_message_id.to_string(),
            },
        );
        start_gate.notify_one();
    }

    async fn handle_stream_terminal_state(
        &self,
        kind: StreamJobKind,
        session_id: String,
        message_id: MessageId,
        result: StreamDriveResult,
    ) {
        match result {
            StreamDriveResult::Completed(completed) => {
                let CompletedStreamOutput {
                    session_id: _completed_session,
                    call_id,
                    script,
                    invalid_script,
                    protocol_error,
                } = *completed;
                let completed_session = {
                    let agent = self.agent_manager.read().await;
                    agent.complete_message(&message_id).await
                };
                let completed_session = match completed_session {
                    Ok(Some(completed_session)) => completed_session,
                    Ok(None) => return,
                    Err(error) => {
                        self.handle_completion_persistence_failure(
                            session_id,
                            message_id,
                            error,
                            kind.is_continuation(),
                        )
                        .await;
                        return;
                    }
                };

                emit_event(
                    &self.event_tx,
                    Event::StreamComplete {
                        session_id: completed_session.clone(),
                        message_id: message_id.to_string(),
                    },
                );
                emit_event(
                    &self.event_tx,
                    Event::HistoryUpdated {
                        session_id: completed_session.clone(),
                    },
                );
                self.process_completed_stream_output(
                    completed_session,
                    message_id,
                    call_id,
                    script,
                    invalid_script,
                    protocol_error,
                )
                .await;
            }
            StreamDriveResult::FailedToStart { error } => {
                match self
                    .abort_stream_for_recovery(&session_id, &message_id)
                    .await
                {
                    Ok(true) => {
                        self.schedule_queue_drain(&session_id);
                        if kind.is_continuation() {
                            emit_event(
                                &self.event_tx,
                                Event::HistoryUpdated {
                                    session_id: session_id.clone(),
                                },
                            );
                        }
                    }
                    Ok(false) => {
                        let recovery_target = if kind.is_continuation() {
                            "continuation stream"
                        } else {
                            "stream"
                        };
                        self.send_session_error(
                            &session_id,
                            format!(
                                "Failed to recover {recovery_target} {} after start failure",
                                message_id
                            ),
                        );
                    }
                    Err(rollback_error) => {
                        let recovery_target = if kind.is_continuation() {
                            "continuation stream"
                        } else {
                            "stream"
                        };
                        self.send_session_error(
                            &session_id,
                            format!(
                                "Failed to roll back {recovery_target} {} after start failure: {rollback_error}",
                                message_id
                            ),
                        );
                    }
                }

                if kind.is_continuation() {
                    emit_event(
                        &self.event_tx,
                        Event::ContinuationFailed { session_id, error },
                    );
                } else {
                    emit_event(
                        &self.event_tx,
                        Event::StreamError {
                            session_id,
                            message_id: message_id.to_string(),
                            error,
                        },
                    );
                }
            }
            StreamDriveResult::FailedDuringStream { error } => {
                match self
                    .abort_stream_for_recovery(&session_id, &message_id)
                    .await
                {
                    Ok(true) => {
                        self.schedule_queue_drain(&session_id);
                    }
                    Ok(false) => {
                        let recovery_target = if kind.is_continuation() {
                            "continuation stream"
                        } else {
                            "stream"
                        };
                        self.send_session_error(
                            &session_id,
                            format!(
                                "Failed to recover {recovery_target} {} after runtime error",
                                message_id
                            ),
                        );
                    }
                    Err(rollback_error) => {
                        let recovery_target = if kind.is_continuation() {
                            "continuation stream"
                        } else {
                            "stream"
                        };
                        self.send_session_error(
                            &session_id,
                            format!(
                                "Failed to roll back {recovery_target} {} after runtime error: {rollback_error}",
                                message_id
                            ),
                        );
                    }
                }
                if kind.is_continuation() {
                    tracing::error!("Continuation stream error: {error}");
                }
                emit_event(
                    &self.event_tx,
                    Event::StreamError {
                        session_id,
                        message_id: message_id.to_string(),
                        error,
                    },
                );
            }
            StreamDriveResult::Stopped => {}
        }
    }

    async fn handle_completion_persistence_failure(
        &self,
        session_id: String,
        message_id: MessageId,
        error: color_eyre::Report,
        continuation_error: bool,
    ) {
        let rollback_result = {
            let mut agent = self.agent_manager.write().await;
            let rollback_result = agent.abort_streaming_message(&message_id).await;
            if rollback_result.is_ok() {
                agent.clear_active_turn(&session_id);
                drop(agent);
                self.release_turn(&session_id).await;
                self.event_tx.finish_timer(&session_id);
            }
            rollback_result
        };

        match rollback_result {
            Ok(Some(_)) => {
                self.schedule_queue_drain(&session_id);
                emit_event(
                    &self.event_tx,
                    Event::HistoryUpdated {
                        session_id: session_id.clone(),
                    },
                );
            }
            Ok(None) => {
                self.send_session_error(
                    &session_id,
                    format!(
                        "Failed to recover stream state for message {} after completion error",
                        message_id
                    ),
                );
            }
            Err(rollback_error) => {
                if matches!(self.session_store.owns_turn(&session_id).await, Ok(false)) {
                    self.finish_turn(&session_id).await;
                }
                self.send_session_error(
                    &session_id,
                    format!(
                        "Failed to roll back stream {} after completion error: {rollback_error}",
                        message_id
                    ),
                );
            }
        }

        if continuation_error {
            emit_event(
                &self.event_tx,
                Event::ContinuationFailed {
                    session_id,
                    error: error.to_string(),
                },
            );
        } else {
            emit_event(
                &self.event_tx,
                Event::StreamError {
                    session_id,
                    message_id: message_id.to_string(),
                    error: error.to_string(),
                },
            );
        }
    }

    async fn abort_stream_for_recovery(
        &self,
        session_id: &str,
        message_id: &MessageId,
    ) -> Result<bool> {
        let mut agent = self.agent_manager.write().await;
        let rollback_result = agent.abort_streaming_message(message_id).await;
        if rollback_result.is_err() {
            drop(agent);
            if !self.session_store.owns_turn(session_id).await? {
                self.finish_turn(session_id).await;
                return Ok(true);
            }
            return rollback_result.map(|_| false);
        }
        let rollback_result = rollback_result?;
        if rollback_result.is_some() {
            agent.clear_active_turn(session_id);
            drop(agent);
            self.release_turn(session_id).await;
            self.event_tx.finish_timer(session_id);
            Ok(true)
        } else {
            drop(agent);
            Ok(false)
        }
    }

    pub(crate) async fn clear_active_stream(
        &self,
        session_id: &str,
        message_id: &MessageId,
    ) -> bool {
        let mut active_streams = self.active_streams.lock().await;
        let should_remove = active_streams
            .get(session_id)
            .is_some_and(|stream| &stream.message_id == message_id);
        if should_remove {
            active_streams.remove(session_id);
        }
        should_remove
    }

    pub(crate) async fn take_active_stream(&self, session_id: &str) -> Option<ActiveStream> {
        self.active_streams.lock().await.remove(session_id)
    }

    pub(crate) async fn cancel_stream(&self, session_id: String) -> Result<bool> {
        self.cancel_session_work(session_id, false, None).await
    }

    pub(crate) async fn cancel_turn(&self, session_id: String) -> Result<bool> {
        self.cancel_session_work(session_id, true, None).await
    }

    pub(crate) async fn cancel_turn_if_lease_matches(
        &self,
        session_id: String,
        expected: i64,
    ) -> Result<bool> {
        self.cancel_session_work(session_id, true, Some(expected))
            .await
    }

    async fn cancel_session_work(
        &self,
        session_id: String,
        discard_queued: bool,
        expected: Option<i64>,
    ) -> Result<bool> {
        let state_guard = self.session_state_barrier.read().await;
        let preparation = self.session_preparations.begin(&session_id).await;
        if let Some(expected) = expected
            && !self
                .session_store
                .lease_token_matches(&session_id, expected)
                .await?
        {
            return Ok(false);
        }
        let discarded = discard_queued && !self.take_queued_messages(&session_id).await.is_empty();
        if self.cancel_pending_script(&session_id).await? {
            return Ok(true);
        }
        let Some(active_stream) = self.take_active_stream(&session_id).await else {
            let completion = {
                let tasks = self.active_script_tasks.lock().await;
                tasks.get(&session_id).map(|task| {
                    task.cancellation.cancel();
                    task.completion.clone()
                })
            };
            let mut agent = self.agent_manager.write().await;
            let active = agent.is_turn_active(&session_id);
            if active {
                agent.clear_active_turn(&session_id);
            }
            drop(agent);
            let cancelled = active || completion.is_some() || discarded;
            if completion.is_none() && cancelled {
                self.release_turn(&session_id).await;
            }
            drop(preparation);
            drop(state_guard);
            if let Some(completion) = completion {
                completion.cancelled().await;
            }
            if cancelled {
                let _state_guard = self.session_state_barrier.read().await;
                self.event_tx.finish_timer(&session_id);
                self.send_event(Event::HistoryUpdated {
                    session_id: session_id.clone(),
                });
                self.schedule_queue_drain(&session_id);
            }
            return Ok(cancelled);
        };

        let cancelled_stream = {
            let mut agent = self.agent_manager.write().await;
            active_stream.abort_handle.abort();
            let cancelled_script_output = kraai_script_protocol::render_tool_call_result(
                kraai_script_protocol::ToolCallResultView {
                    status: kraai_types::ScriptExecutionStatus::Cancelled,
                    exit_code: None,
                    elapsed_millis: None,
                    stdout: &[],
                    stderr: &[],
                    diagnostic: Some("Script was cancelled before execution"),
                },
            );
            let cancelled = match agent
                .cancel_streaming_message(&active_stream.message_id, &cancelled_script_output)
                .await
            {
                Ok(cancelled) => cancelled,
                Err(error) => {
                    drop(agent);
                    if !self.session_store.owns_turn(&session_id).await? {
                        self.stream_tasks.wait_session(&session_id).await;
                        self.finish_turn(&session_id).await;
                        self.send_session_error(&session_id, error);
                        return Ok(true);
                    }
                    self.active_streams
                        .lock()
                        .await
                        .entry(session_id)
                        .or_insert(active_stream);
                    return Err(error);
                }
            };
            agent.clear_active_turn(&session_id);
            drop(agent);
            if cancelled.is_some() {
                self.stream_tasks.wait_session(&session_id).await;
            }
            self.release_turn(&session_id).await;
            self.event_tx.finish_timer(&session_id);
            cancelled
        };
        let Some(cancelled_stream) = cancelled_stream else {
            return Ok(discarded);
        };

        self.send_event(Event::StreamCancelled {
            session_id: cancelled_stream.session_id.clone(),
            message_id: cancelled_stream.message_id.to_string(),
        });
        self.send_event(Event::HistoryUpdated {
            session_id: cancelled_stream.session_id,
        });
        self.schedule_queue_drain(&session_id);
        Ok(true)
    }
}
