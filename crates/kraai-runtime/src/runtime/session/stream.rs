use std::panic::AssertUnwindSafe;

use futures::FutureExt;
use kraai_agent::PendingStreamRequest;
use kraai_provider_core::ProviderManager;
use kraai_types::{AssistantPhase, MessageId, TokenUsage, ToolCallId};
use tokio_util::sync::CancellationToken;

use super::super::core::RuntimeCore;
use super::super::stream_driver::{CompletedStreamOutput, StreamDriveResult, StreamReader};
use super::actor::{SessionActor, SessionMessage, StreamJob, StreamKind, TurnState};
use crate::api::Event;
use kraai_types::SessionId;

/// A provider stream change that the actor applies to the streaming message.
#[derive(Debug)]
pub(crate) enum StreamEvent {
    Text {
        item_id: String,
        phase: AssistantPhase,
        delta: String,
    },
    Reasoning {
        provider_id: kraai_types::ProviderId,
        payload: serde_json::Value,
    },
    ReasoningDelta {
        item_id: String,
        delta: String,
    },
    ScriptCall {
        call_id: ToolCallId,
        name: String,
        input: String,
        arguments_error: Option<String>,
    },
    Usage(TokenUsage),
    /// The request failed transiently and is being issued again.
    Restart {
        reason: String,
    },
}

impl SessionActor {
    pub(super) fn start_stream(
        &mut self,
        kind: StreamKind,
        providers: ProviderManager,
        mut request: PendingStreamRequest,
    ) {
        let generation = self.generation();
        let context_notifications = std::mem::take(&mut request.context_notifications);
        let message_id = request.message_id.clone();
        let cancel = CancellationToken::new();
        let reader = StreamReader {
            session_id: self.session_id.clone(),
            providers,
            agent_manager: self.runtime.agent_manager.clone(),
            session_store: self.runtime.session_store.clone(),
            event_tx: self.runtime.event_tx.clone(),
            barrier: self.runtime.sessions.barrier(&self.session_id),
            auxiliary_usage_task: self.runtime.stream_tasks.auxiliary_token(&self.session_id),
            mailbox: self.mailbox(),
            generation,
        };
        self.runtime.send_event(Event::StreamStart {
            session_id: self.session_id.clone(),
            message_id: message_id.to_string(),
        });
        if !context_notifications.is_empty() {
            self.runtime.send_event(Event::ContextStateChanged {
                session_id: self.session_id.clone(),
                notifications: context_notifications,
            });
        }
        let stream_tasks = self.runtime.stream_tasks.clone();
        let session_id = self.session_id.clone();
        let mailbox = self.mailbox();
        let reader_cancel = cancel.clone();
        let task = self
            .runtime
            .stream_tasks
            .spawn(&self.session_id, async move {
                let result = tokio::select! {
                    () = reader_cancel.cancelled() => StreamDriveResult::Stopped,
                    result = AssertUnwindSafe(reader.drive(request)).catch_unwind() => {
                        result.unwrap_or_else(|_| StreamDriveResult::FailedDuringStream {
                            error: String::from("provider stream task panicked"),
                        })
                    }
                };
                stream_tasks.wait_auxiliary_session(&session_id).await;
                let _ = mailbox.send(SessionMessage::StreamEnded { generation, result });
            });
        self.state = TurnState::Streaming(StreamJob {
            generation,
            kind,
            message_id,
            cancel,
            task,
        });
    }

    pub(super) async fn stream_event(&mut self, generation: u64, event: StreamEvent) {
        let TurnState::Streaming(job) = &self.state else {
            return;
        };
        if job.generation != generation {
            return;
        }
        let message_id = job.message_id.clone();
        let failure = apply_stream_event(&self.runtime, &self.session_id, &message_id, event).await;
        if let Some(error) = failure
            && let TurnState::Streaming(job) = std::mem::replace(&mut self.state, TurnState::Idle)
        {
            job.cancel.cancel();
            self.stream_failed(job.kind, job.message_id, error, false)
                .await;
        }
    }

    pub(super) async fn stream_ended(&mut self, generation: u64, result: StreamDriveResult) {
        let job = match std::mem::replace(&mut self.state, TurnState::Idle) {
            TurnState::Streaming(job) if job.generation == generation => job,
            state => {
                self.state = state;
                return;
            }
        };
        if self.stopping {
            return;
        }
        let StreamJob {
            kind, message_id, ..
        } = job;
        match result {
            StreamDriveResult::Completed(completed) => {
                self.stream_completed(kind, message_id, *completed).await;
            }
            StreamDriveResult::FailedToStart { error } => {
                self.stream_failed(kind, message_id, error, true).await;
            }
            StreamDriveResult::FailedDuringStream { error } => {
                self.stream_failed(kind, message_id, error, false).await;
            }
            StreamDriveResult::Stopped => {
                self.stream_failed(
                    kind,
                    message_id,
                    String::from("Provider stream stopped unexpectedly"),
                    false,
                )
                .await;
            }
        }
    }

    async fn stream_completed(
        &mut self,
        kind: StreamKind,
        message_id: MessageId,
        completed: CompletedStreamOutput,
    ) {
        let CompletedStreamOutput {
            call_id,
            script,
            invalid_script,
            protocol_error,
            reached_output_limit,
            ..
        } = completed;
        match self
            .runtime
            .agent_manager
            .complete_message(&message_id)
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => return,
            Err(error) => {
                self.completion_persistence_failed(kind, message_id, error.into())
                    .await;
                return;
            }
        }
        self.runtime.send_event(Event::StreamComplete {
            session_id: self.session_id.clone(),
            message_id: message_id.to_string(),
        });
        if reached_output_limit {
            self.runtime.send_event(Event::ContextStateChanged {
                session_id: self.session_id.clone(),
                notifications: vec![String::from(
                    "The response stopped at the model's output token limit.",
                )],
            });
        }
        self.process_completed_stream_output(
            message_id,
            call_id,
            script,
            invalid_script,
            protocol_error,
        )
        .await;
    }

    /// Rolls back a failed stream's assistant message and reports the failure.
    pub(super) async fn stream_failed(
        &mut self,
        kind: StreamKind,
        message_id: MessageId,
        error: String,
        before_start: bool,
    ) {
        let recovery_target = match kind {
            StreamKind::Continuation => "continuation stream",
            StreamKind::Initial => "stream",
        };
        let moment = if before_start {
            "start failure"
        } else {
            "runtime error"
        };
        match self.abort_stream_for_recovery(&message_id).await {
            Ok(true) => self.drain().await,
            Ok(false) => self.runtime.send_session_error(
                &self.session_id,
                format!("Failed to recover {recovery_target} {message_id} after {moment}"),
            ),
            Err(rollback_error) => self.runtime.send_session_error(
                &self.session_id,
                format!(
                    "Failed to roll back {recovery_target} {message_id} after {moment}: {rollback_error}"
                ),
            ),
        }
        if kind == StreamKind::Continuation {
            tracing::error!("Continuation stream error: {error}");
        }
        if before_start && kind == StreamKind::Continuation {
            self.runtime.send_event(Event::ContinuationFailed {
                session_id: self.session_id.clone(),
                error,
            });
        } else {
            self.runtime.send_event(Event::StreamError {
                session_id: self.session_id.clone(),
                message_id: message_id.to_string(),
                error,
            });
        }
    }

    async fn abort_stream_for_recovery(
        &mut self,
        message_id: &MessageId,
    ) -> color_eyre::Result<bool> {
        match self
            .runtime
            .agent_manager
            .abort_streaming_message(message_id)
            .await
        {
            Ok(Some(_)) => {
                self.finish_turn().await;
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(error) => {
                if !self
                    .runtime
                    .session_store
                    .owns_turn(&self.session_id)
                    .await?
                {
                    self.finish_turn().await;
                    return Ok(true);
                }
                Err(error.into())
            }
        }
    }

    async fn completion_persistence_failed(
        &mut self,
        kind: StreamKind,
        message_id: MessageId,
        error: color_eyre::Report,
    ) {
        match self
            .runtime
            .agent_manager
            .abort_streaming_message(&message_id)
            .await
        {
            Ok(Some(_)) => {
                self.finish_turn().await;
                self.drain().await;
            }
            Ok(None) => self.runtime.send_session_error(
                &self.session_id,
                format!(
                    "Failed to recover stream state for message {message_id} after completion error"
                ),
            ),
            Err(rollback_error) => {
                if matches!(
                    self.runtime.session_store.owns_turn(&self.session_id).await,
                    Ok(false)
                ) {
                    self.finish_turn().await;
                }
                self.runtime.send_session_error(
                    &self.session_id,
                    format!(
                        "Failed to roll back stream {message_id} after completion error: {rollback_error}"
                    ),
                );
            }
        }
        if kind == StreamKind::Continuation {
            self.runtime.send_event(Event::ContinuationFailed {
                session_id: self.session_id.clone(),
                error: error.to_string(),
            });
        } else {
            self.runtime.send_event(Event::StreamError {
                session_id: self.session_id.clone(),
                message_id: message_id.to_string(),
                error: error.to_string(),
            });
        }
    }
}

fn send_streamed_item(
    runtime: &RuntimeCore,
    session_id: &SessionId,
    message_id: &MessageId,
    streamed: Option<kraai_agent::StreamedItem>,
) {
    let event = match streamed {
        Some(kraai_agent::StreamedItem::Started { index, item }) => Event::ItemStarted {
            session_id: session_id.clone(),
            message_id: message_id.to_string(),
            item_index: index,
            item: super::super::client_view::client_item(item),
        },
        Some(kraai_agent::StreamedItem::Delta { index, delta }) if !delta.is_empty() => {
            Event::ItemDelta {
                session_id: session_id.clone(),
                message_id: message_id.to_string(),
                item_index: index,
                delta,
            }
        }
        Some(kraai_agent::StreamedItem::Delta { .. }) | None => return,
    };
    runtime.send_event(event);
}

/// Applies one forwarded provider event to the streaming message, returning a failure that
/// ends the stream.
pub(in crate::runtime) async fn apply_stream_event(
    runtime: &RuntimeCore,
    session_id: &SessionId,
    message_id: &MessageId,
    event: StreamEvent,
) -> Option<String> {
    let agent = &runtime.agent_manager;
    match event {
        StreamEvent::Text {
            item_id,
            phase,
            delta,
        } => {
            let streamed = agent
                .append_text_chunk(message_id, &item_id, phase, &delta)
                .await;
            send_streamed_item(runtime, session_id, message_id, streamed);
            None
        }
        StreamEvent::Reasoning {
            provider_id,
            payload,
        } => {
            let streamed = agent
                .append_reasoning(message_id, provider_id, payload)
                .await;
            send_streamed_item(runtime, session_id, message_id, streamed);
            None
        }
        StreamEvent::ReasoningDelta { item_id, delta } => {
            runtime.send_event(Event::ReasoningDelta {
                session_id: session_id.clone(),
                message_id: message_id.to_string(),
                item_id,
                delta,
            });
            None
        }
        StreamEvent::ScriptCall {
            call_id,
            name,
            input,
            arguments_error,
        } => {
            let streamed = agent
                .append_script_call(message_id, call_id, name, input, arguments_error)
                .await;
            send_streamed_item(runtime, session_id, message_id, streamed);
            None
        }
        StreamEvent::Usage(usage) => {
            match agent.set_streaming_message_usage(message_id, usage).await {
                Ok(Some(request)) => {
                    runtime.send_event(Event::RequestUsageUpdated {
                        session_id: session_id.clone(),
                        request: Box::new(request),
                    });
                    None
                }
                Ok(None) => None,
                Err(error) => Some(format!("{error:#}")),
            }
        }
        StreamEvent::Restart { reason } => match agent.reset_streaming_message(message_id).await {
            Ok(reset) => {
                if let Some(Some(request)) = reset {
                    runtime.send_event(Event::RequestUsageUpdated {
                        session_id: session_id.clone(),
                        request: Box::new(request),
                    });
                }
                runtime.send_event(Event::StreamRestarted {
                    session_id: session_id.clone(),
                    message_id: message_id.to_string(),
                    reason,
                });
                None
            }
            Err(error) => Some(format!(
                "{reason}; discarding the partial response failed: {error:#}"
            )),
        },
    }
}
