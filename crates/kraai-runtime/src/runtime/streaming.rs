use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;
use futures::{FutureExt, StreamExt};
use kraai_agent::PendingStreamRequest;
use kraai_provider_core::{
    ProviderManager, ProviderRequestContext, ProviderRetryEvent, ProviderRetryObserver,
    ProviderStreamEvent,
};
use kraai_script_protocol::{InvalidScriptBlock, ProtocolError, ScriptBlock, ScriptProtocolParser};
use kraai_types::{MessageId, ModelId, ProviderId};
use tokio::sync::Notify;

use super::core::{ActiveStream, RuntimeCore, emit_event};
use crate::api::Event;

const POST_BOUNDARY_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const POST_BOUNDARY_DRAIN_YIELD_INTERVAL: usize = 256;

struct RuntimeRetryObserver {
    session_id: String,
    provider_id: ProviderId,
    model_id: ModelId,
    event_tx: tokio::sync::broadcast::Sender<Event>,
}

struct CompletedProtocolBoundary {
    script: Option<ScriptBlock>,
    invalid_script: Option<InvalidScriptBlock>,
    protocol_error: Option<ProtocolError>,
}

impl ProviderRetryObserver for RuntimeRetryObserver {
    fn on_retry_scheduled(&self, event: &ProviderRetryEvent) {
        emit_event(
            &self.event_tx,
            Event::ProviderRetryScheduled {
                session_id: self.session_id.clone(),
                provider_id: self.provider_id.to_string(),
                model_id: self.model_id.to_string(),
                operation: event.operation.to_string(),
                retry_number: event.retry_number,
                delay_seconds: event.delay.as_secs(),
                reason: event.reason.clone(),
            },
        );
    }
}

#[derive(Debug)]
pub(crate) enum StreamDriveResult {
    Completed {
        session_id: String,
        content: String,
        script: Option<ScriptBlock>,
        invalid_script: Option<InvalidScriptBlock>,
        protocol_error: Option<ProtocolError>,
    },
    FailedToStart {
        error: String,
    },
    FailedDuringStream {
        error: String,
    },
    Stopped,
}

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
    pub(crate) async fn start_continuation(&self, session_id: String) {
        if self
            .pending_script_approvals
            .lock()
            .await
            .contains_key(&session_id)
            || self.has_active_script_tasks(&session_id).await
        {
            return;
        }
        let continuation = {
            let mut agent = self.agent_manager.lock().await;
            match agent.prepare_continuation_stream(&session_id).await {
                Ok(result) => Ok(result.map(|request| (agent.cloned_provider_manager(), request))),
                Err(error) => Err(error),
            }
        };

        match continuation {
            Ok(Some((providers, request))) => {
                self.start_stream_job(StreamJobKind::Continuation, session_id, providers, request)
                    .await;
            }
            Ok(None) => {}
            Err(error) => {
                {
                    let mut agent = self.agent_manager.lock().await;
                    agent.clear_active_turn(&session_id);
                }
                self.schedule_queue_drain(&session_id).await;
                emit_event(
                    &self.event_tx,
                    Event::HistoryUpdated {
                        session_id: session_id.clone(),
                    },
                );
                emit_event(
                    &self.event_tx,
                    Event::ContinuationFailed {
                        session_id,
                        error: error.to_string(),
                    },
                );
            }
        }
    }

    pub(crate) fn spawn_continuation(&self, session_id: String) {
        let runtime = self.clone();
        tokio::spawn(async move {
            runtime.start_continuation(session_id).await;
        });
    }

    pub(crate) async fn start_stream_job(
        &self,
        kind: StreamJobKind,
        session_id: String,
        providers: ProviderManager,
        request: PendingStreamRequest,
    ) {
        let runtime = self.clone();
        let start_gate = Arc::new(Notify::new());
        let request_session_id = session_id.clone();
        let request_message_id = request.message_id.clone();
        let active_message_id = request_message_id.clone();
        let terminal_message_id = request_message_id.clone();
        let task_runtime = runtime.clone();

        let task = tokio::spawn({
            let start_gate = start_gate.clone();
            async move {
                start_gate.notified().await;
                let result = AssertUnwindSafe(RuntimeCore::drive_stream(
                    request_session_id.clone(),
                    request,
                    providers,
                    task_runtime.agent_manager.clone(),
                    task_runtime.event_tx.clone(),
                ))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| StreamDriveResult::FailedDuringStream {
                    error: String::from("provider stream task panicked"),
                });

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

        let previous = self.active_streams.lock().await.insert(
            session_id,
            ActiveStream {
                message_id: request_message_id,
                abort_handle: task.abort_handle(),
            },
        );
        if let Some(previous) = previous {
            previous.abort_handle.abort();
        }
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
            StreamDriveResult::Completed {
                session_id: _completed_session,
                content,
                script,
                invalid_script,
                protocol_error,
            } => {
                let completed_session = {
                    let agent = self.agent_manager.lock().await;
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
                    content,
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
                        self.schedule_queue_drain(&session_id).await;
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
                        emit_event(
                            &self.event_tx,
                            Event::Error(format!(
                                "Failed to recover {recovery_target} {} after start failure",
                                message_id
                            )),
                        );
                    }
                    Err(rollback_error) => {
                        let recovery_target = if kind.is_continuation() {
                            "continuation stream"
                        } else {
                            "stream"
                        };
                        emit_event(
                            &self.event_tx,
                            Event::Error(format!(
                                "Failed to roll back {recovery_target} {} after start failure: {rollback_error}",
                                message_id
                            )),
                        );
                    }
                }

                if kind.is_continuation() {
                    emit_event(
                        &self.event_tx,
                        Event::ContinuationFailed { session_id, error },
                    );
                } else {
                    emit_event(&self.event_tx, Event::Error(error));
                }
            }
            StreamDriveResult::FailedDuringStream { error } => {
                match self
                    .abort_stream_for_recovery(&session_id, &message_id)
                    .await
                {
                    Ok(true) => {
                        self.schedule_queue_drain(&session_id).await;
                    }
                    Ok(false) => {
                        let recovery_target = if kind.is_continuation() {
                            "continuation stream"
                        } else {
                            "stream"
                        };
                        emit_event(
                            &self.event_tx,
                            Event::Error(format!(
                                "Failed to recover {recovery_target} {} after runtime error",
                                message_id
                            )),
                        );
                    }
                    Err(rollback_error) => {
                        let recovery_target = if kind.is_continuation() {
                            "continuation stream"
                        } else {
                            "stream"
                        };
                        emit_event(
                            &self.event_tx,
                            Event::Error(format!(
                                "Failed to roll back {recovery_target} {} after runtime error: {rollback_error}",
                                message_id
                            )),
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
            let mut agent = self.agent_manager.lock().await;
            let rollback_result = agent.abort_streaming_message(&message_id).await;
            if rollback_result.is_ok() {
                agent.clear_active_turn(&session_id);
            }
            rollback_result
        };

        match rollback_result {
            Ok(Some(_)) => {
                self.schedule_queue_drain(&session_id).await;
                emit_event(
                    &self.event_tx,
                    Event::HistoryUpdated {
                        session_id: session_id.clone(),
                    },
                );
            }
            Ok(None) => {
                emit_event(
                    &self.event_tx,
                    Event::Error(format!(
                        "Failed to recover stream state for message {} after completion error",
                        message_id
                    )),
                );
            }
            Err(rollback_error) => {
                emit_event(
                    &self.event_tx,
                    Event::Error(format!(
                        "Failed to roll back stream {} after completion error: {rollback_error}",
                        message_id
                    )),
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
        let mut agent = self.agent_manager.lock().await;
        let rollback_result = agent.abort_streaming_message(message_id).await?;
        if rollback_result.is_some() {
            agent.clear_active_turn(session_id);
            drop(agent);
            Ok(true)
        } else {
            drop(agent);
            Ok(false)
        }
    }

    pub(crate) async fn drive_stream(
        session_id: String,
        request: PendingStreamRequest,
        providers: ProviderManager,
        agent_manager: Arc<tokio::sync::Mutex<kraai_agent::AgentManager>>,
        event_tx: tokio::sync::broadcast::Sender<Event>,
    ) -> StreamDriveResult {
        let PendingStreamRequest {
            message_id,
            provider_id,
            model_id,
            provider_messages,
            context_notifications,
        } = request;
        if !context_notifications.is_empty() {
            emit_event(
                &event_tx,
                Event::ContextStateChanged {
                    session_id: session_id.clone(),
                    notifications: context_notifications,
                },
            );
        }
        let request_context = ProviderRequestContext::with_retry_observer_and_prompt_cache_key(
            Arc::new(RuntimeRetryObserver {
                session_id: session_id.clone(),
                provider_id: provider_id.clone(),
                model_id: model_id.clone(),
                event_tx: event_tx.clone(),
            }),
            session_id.clone(),
        );
        let mut stream = match providers
            .generate_reply_stream(provider_id, &model_id, provider_messages, request_context)
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                return StreamDriveResult::FailedToStart {
                    error: error.to_string(),
                };
            }
        };

        emit_event(
            &event_tx,
            Event::StreamStart {
                session_id: session_id.clone(),
                message_id: message_id.to_string(),
            },
        );

        let mut content = String::new();
        let mut parser = ScriptProtocolParser::new();
        let mut completed_boundary = None;
        let mut post_boundary_deadline = None;
        let mut post_boundary_events = 0_usize;

        loop {
            let next_event = if let Some(deadline) = post_boundary_deadline {
                match tokio::time::timeout_at(deadline, stream.next()).await {
                    Ok(event) => event,
                    Err(_) => {
                        tracing::warn!(
                            timeout_millis = POST_BOUNDARY_DRAIN_TIMEOUT.as_millis(),
                            "Stopping provider stream drain after timeout"
                        );
                        break;
                    }
                }
            } else {
                stream.next().await
            };
            let Some(chunk_result) = next_event else {
                break;
            };
            let draining_after_boundary = completed_boundary.is_some();
            if draining_after_boundary {
                post_boundary_events = post_boundary_events.saturating_add(1);
                if post_boundary_events.is_multiple_of(POST_BOUNDARY_DRAIN_YIELD_INTERVAL) {
                    tokio::task::yield_now().await;
                }
            }
            match chunk_result {
                Ok(ProviderStreamEvent::TextDelta(chunk)) => {
                    if completed_boundary.is_some() {
                        tracing::trace!(
                            discarded_bytes = chunk.len(),
                            "Discarding text after script protocol boundary"
                        );
                        continue;
                    }
                    let parsed = parser.ingest(&chunk);
                    if !parsed.accepted.is_empty() {
                        content.push_str(&parsed.accepted);
                        {
                            let agent = agent_manager.lock().await;
                            if !agent.append_chunk(&message_id, &parsed.accepted).await {
                                return StreamDriveResult::Stopped;
                            }
                        }
                        emit_event(
                            &event_tx,
                            Event::StreamChunk {
                                session_id: session_id.clone(),
                                message_id: message_id.to_string(),
                                chunk: parsed.accepted,
                            },
                        );
                    }
                    if parsed.should_stop {
                        let invalid_script = parsed.error.as_ref().map(|_| parser.invalid_block());
                        tracing::debug!(
                            "Script protocol boundary reached; draining provider stream for usage"
                        );
                        completed_boundary = Some(CompletedProtocolBoundary {
                            script: parsed.completed,
                            invalid_script,
                            protocol_error: parsed.error,
                        });
                        post_boundary_deadline =
                            Some(tokio::time::Instant::now() + POST_BOUNDARY_DRAIN_TIMEOUT);
                    }
                }
                Ok(ProviderStreamEvent::Usage(usage)) => {
                    let agent = agent_manager.lock().await;
                    if !agent.set_streaming_message_usage(&message_id, usage).await {
                        return StreamDriveResult::Stopped;
                    }
                    drop(agent);
                    if draining_after_boundary {
                        tracing::debug!(
                            drained_events = post_boundary_events,
                            "Provider usage received after script protocol boundary"
                        );
                        break;
                    }
                }
                Err(error) => {
                    if completed_boundary.is_some() {
                        tracing::warn!(
                            error = %error,
                            "Stopping provider stream drain after error"
                        );
                        break;
                    }
                    return StreamDriveResult::FailedDuringStream {
                        error: error.to_string(),
                    };
                }
            }
        }

        if let Some(boundary) = completed_boundary {
            tracing::debug!(
                drained_events = post_boundary_events,
                "Provider stream drain finished after script protocol boundary"
            );
            return StreamDriveResult::Completed {
                session_id,
                content,
                script: boundary.script,
                invalid_script: boundary.invalid_script,
                protocol_error: boundary.protocol_error,
            };
        }

        let tail = parser.finish();
        if !tail.accepted.is_empty() {
            content.push_str(&tail.accepted);
            {
                let agent = agent_manager.lock().await;
                if !agent.append_chunk(&message_id, &tail.accepted).await {
                    return StreamDriveResult::Stopped;
                }
            }
            emit_event(
                &event_tx,
                Event::StreamChunk {
                    session_id: session_id.clone(),
                    message_id: message_id.to_string(),
                    chunk: tail.accepted,
                },
            );
        }

        tracing::debug!("Full content length: {}", content.len());

        let invalid_script = tail.error.as_ref().map(|_| parser.invalid_block());
        StreamDriveResult::Completed {
            session_id,
            content,
            script: tail.completed,
            invalid_script,
            protocol_error: tail.error,
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
        let Some(active_stream) = self.take_active_stream(&session_id).await else {
            return Ok(self.cancel_active_script(&session_id).await);
        };

        active_stream.abort_handle.abort();

        let cancelled_stream = {
            let mut agent = self.agent_manager.lock().await;
            let cancelled = match agent
                .cancel_streaming_message(&active_stream.message_id)
                .await
            {
                Ok(cancelled) => cancelled,
                Err(error) => {
                    drop(agent);
                    self.active_streams
                        .lock()
                        .await
                        .entry(session_id)
                        .or_insert(active_stream);
                    return Err(error);
                }
            };
            agent.clear_active_turn(&session_id);
            cancelled
        };
        let Some(cancelled_stream) = cancelled_stream else {
            return Ok(false);
        };

        self.send_event(Event::StreamCancelled {
            session_id: cancelled_stream.session_id.clone(),
            message_id: cancelled_stream.message_id.to_string(),
        });
        self.send_event(Event::HistoryUpdated {
            session_id: cancelled_stream.session_id,
        });
        self.schedule_queue_drain(&session_id).await;
        Ok(true)
    }
}
