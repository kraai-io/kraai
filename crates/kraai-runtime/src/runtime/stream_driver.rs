use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use futures::stream::BoxStream;
use kraai_agent::{ContextCompaction, PendingStreamRequest};
use kraai_execution_types::{SandboxCapabilities, ScriptRewrites};
use kraai_provider_core::{
    ProviderManager, ProviderRequest, ProviderRequestContext, ProviderStreamEvent,
    ScriptToolDefinition,
};
use kraai_script_protocol::{InvalidScriptBlock, ProtocolError, ScriptBlock, parse_script_input};
use kraai_types::{ConversationItem, MessageId, ModelId, ProviderId, ScriptCall, SessionId};
use tokio::sync::{RwLock, mpsc};

use super::core::emit_event;
use super::request_usage::RuntimeRetryObserver;
use super::session::{SessionMessage, StreamEvent};
use crate::api::Event;
use crate::handle::RuntimeEventSender;

const MAX_STREAM_REISSUES: u32 = 2;
pub(super) const POST_BOUNDARY_DRAIN_YIELD_INTERVAL: usize = 256;

struct CompletedProtocolBoundary {
    call: ScriptCall,
    script: Option<ScriptBlock>,
    invalid_script: Option<InvalidScriptBlock>,
    protocol_error: Option<ProtocolError>,
    rewrites: ScriptRewrites,
}

#[derive(Debug)]
pub(super) enum StreamDriveResult {
    Completed(Box<CompletedStreamOutput>),
    FailedToStart { error: String },
    FailedDuringStream { error: String },
    Stopped,
}

#[derive(Debug)]
pub(super) struct CompletedStreamOutput {
    pub(super) call: Option<ScriptCall>,
    pub(super) script: Option<ScriptBlock>,
    pub(super) invalid_script: Option<InvalidScriptBlock>,
    pub(super) protocol_error: Option<ProtocolError>,
    pub(super) rewrites: ScriptRewrites,
    pub(super) reached_output_limit: bool,
}

pub(super) fn auxiliary_usage_observer(
    event_tx: RuntimeEventSender,
    session_id: SessionId,
    task: super::stream_tasks::SessionTaskToken,
) -> Arc<dyn Fn(kraai_types::RequestUsage) + Send + Sync> {
    Arc::new(move |request| {
        let _task = &task;
        emit_event(
            &event_tx,
            Event::RequestUsageUpdated {
                session_id: session_id.clone(),
                request: Box::new(request),
            },
        );
    })
}

pub(super) struct StreamReader {
    pub(super) session_id: SessionId,
    pub(super) providers: ProviderManager,
    pub(super) agent_manager: Arc<kraai_agent::AgentManager>,
    pub(super) session_store: Arc<kraai_persistence::SqliteSessionStore>,
    pub(super) event_tx: RuntimeEventSender,
    pub(super) barrier: Arc<RwLock<()>>,
    pub(super) auxiliary_usage_task: super::stream_tasks::SessionTaskToken,
    pub(super) mailbox: mpsc::UnboundedSender<SessionMessage>,
    pub(super) generation: u64,
}

struct ResponseRequest {
    message_id: MessageId,
    provider_id: ProviderId,
    model_id: ModelId,
    provider_request: ProviderRequest,
    context: ProviderRequestContext,
    compaction: Option<ContextCompaction>,
}

impl StreamReader {
    pub(super) async fn drive(self, request: PendingStreamRequest) -> StreamDriveResult {
        let PendingStreamRequest {
            message_id,
            provider_id,
            model_id,
            provider_request,
            context_notifications: _,
            context_compaction,
            compact_first,
        } = request;
        let on_auxiliary_usage = auxiliary_usage_observer(
            self.event_tx.clone(),
            self.session_id.clone(),
            self.auxiliary_usage_task.clone(),
        );
        let mut compaction = context_compaction.map(|compaction| {
            compaction
                .observe_usage(self.barrier.clone(), on_auxiliary_usage)
                .with_image_resolver(self.image_resolver())
        });
        let provider_request = match compaction.take_if(|_| compact_first) {
            Some(compaction) => {
                match self
                    .compact(&compaction, &provider_request, &provider_id, &model_id)
                    .await
                {
                    Ok(request) => request,
                    Err(error) => return StreamDriveResult::FailedToStart { error },
                }
            }
            None => provider_request,
        };
        log_file_contexts(&self.session_id, &message_id, &provider_request);
        if let Err(error) = self.record_request_started(&message_id).await {
            return StreamDriveResult::FailedToStart { error };
        }
        let context = ProviderRequestContext::with_retry_observer_and_prompt_cache_key(
            Arc::new(RuntimeRetryObserver {
                session_id: self.session_id.clone(),
                provider_id: provider_id.clone(),
                model_id: model_id.clone(),
                event_tx: self.event_tx.clone(),
                message_id: message_id.clone(),
                agent_manager: Arc::clone(&self.agent_manager),
                session_state_barrier: Arc::clone(&self.barrier),
            }),
            self.session_id.to_string(),
        )
        .with_image_resolver(self.image_resolver());
        self.read(ResponseRequest {
            message_id,
            provider_id,
            model_id,
            provider_request,
            context,
            compaction,
        })
        .await
    }

    fn image_resolver(&self) -> Arc<super::images::StoredImageResolver> {
        Arc::new(super::images::StoredImageResolver(
            self.agent_manager.image_store(),
        ))
    }

    fn notify(&self, notification: String) {
        emit_event(
            &self.event_tx,
            Event::ContextStateChanged {
                session_id: self.session_id.clone(),
                notifications: vec![notification],
            },
        );
    }

    fn forward(&self, event: StreamEvent) -> bool {
        self.mailbox
            .send(SessionMessage::Stream {
                generation: self.generation,
                event,
            })
            .is_ok()
    }

    async fn compact(
        &self,
        compaction: &ContextCompaction,
        original: &ProviderRequest,
        provider_id: &ProviderId,
        model_id: &ModelId,
    ) -> Result<ProviderRequest, String> {
        self.notify(String::from("Compacting conversation context."));
        match compaction
            .run(original, &self.providers, provider_id, model_id)
            .await
        {
            Ok(outcome) => {
                self.notify(outcome.notification);
                Ok(outcome.request)
            }
            Err(error) => {
                self.notify(String::from("Conversation context compaction failed."));
                Err(format!("{error:#}"))
            }
        }
    }

    async fn record_request_started(&self, message_id: &MessageId) -> Result<(), String> {
        let _state_guard = self.barrier.read().await;
        match self.agent_manager.record_request_started(message_id).await {
            Ok(Some(request)) => {
                emit_event(
                    &self.event_tx,
                    Event::RequestUsageUpdated {
                        session_id: self.session_id.clone(),
                        request: Box::new(request),
                    },
                );
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(error) => Err(format!("{error:#}")),
        }
    }

    async fn read(&self, request: ResponseRequest) -> StreamDriveResult {
        let ResponseRequest {
            message_id,
            provider_id,
            model_id,
            mut provider_request,
            context,
            mut compaction,
        } = request;
        let mut response = ResponseState::default();
        let mut reissues = 0_u32;
        let mut warned_about_output_limit = false;
        loop {
            let started = Instant::now();
            let stream = match self
                .providers
                .generate_reply_stream(
                    provider_id.clone(),
                    &model_id,
                    provider_request.clone(),
                    context.clone(),
                )
                .await
            {
                Ok(stream) => stream,
                Err(error) => {
                    if kraai_provider_core::is_context_overflow_error(&error)
                        && let Some(compaction) = compaction.take()
                    {
                        match self
                            .restart(
                                None,
                                Some(&compaction),
                                &mut provider_request,
                                &provider_id,
                                &model_id,
                            )
                            .await
                        {
                            Ok(()) => continue,
                            Err(
                                RestartFailure::Ownership(error)
                                | RestartFailure::Compaction(error),
                            ) => return StreamDriveResult::FailedToStart { error },
                            Err(RestartFailure::Stopped) => return StreamDriveResult::Stopped,
                        }
                    }
                    let error = format!(
                        "Provider request {message_id} failed to start after {} ms: {error:#}",
                        started.elapsed().as_millis()
                    );
                    tracing::error!(request_id = %message_id, provider_id = %provider_id, model_id = %model_id,
                        elapsed_ms = started.elapsed().as_millis(), error = %error,
                        "Provider request failed to start");
                    return StreamDriveResult::FailedToStart { error };
                }
            };
            let end = self
                .read_response(stream, &provider_id, &mut response)
                .await;
            if matches!(end, ResponseEnd::Finished | ResponseEnd::Interrupted { .. }) {
                self.settle_calls(&mut response).await;
            }
            match end {
                // A response cut off before it ran a script would leave the turn with nothing done.
                // When the context left before compaction set the limit, compacting makes room for
                // a retry. A response with neither a call nor text means the provider dropped the
                // cut off call, so it is retried once with a notice. Otherwise the model sees the cut
                // off call as an invalid call, or the cut off text stands.
                ResponseEnd::Finished
                    if response.reached_output_limit && !response.ran_script() =>
                {
                    let model = match self.providers.get_provider(&provider_id) {
                        Some(provider) => provider.get_model(&model_id).await,
                        None => None,
                    };
                    let context_bound = model.as_ref().is_some_and(|model| {
                        model.output_limited_by_context(&provider_request.messages)
                    });
                    let compaction = match compaction.take_if(|_| context_bound) {
                        Some(compaction) => Some(compaction),
                        None if !warned_about_output_limit
                            && response.boundary.is_none()
                            && !response.answered =>
                        {
                            warned_about_output_limit = true;
                            let limit = model
                                .and_then(|model| model.output_limit(&provider_request.messages));
                            provider_request
                                .messages
                                .push(ConversationItem::FileContext {
                                    text: output_limit_notice(limit),
                                });
                            None
                        }
                        None => return response.finish(),
                    };
                    match self
                        .restart(
                            Some(String::from("The response reached the output token limit")),
                            compaction.as_ref(),
                            &mut provider_request,
                            &provider_id,
                            &model_id,
                        )
                        .await
                    {
                        Ok(()) => response = ResponseState::default(),
                        Err(RestartFailure::Compaction(error)) => {
                            tracing::warn!(%error, "Compaction after a cut off response failed; keeping the response");
                            return response.finish();
                        }
                        Err(failure) => return failure.during_stream(),
                    }
                }
                ResponseEnd::Finished => return response.finish(),
                ResponseEnd::Stopped => return StreamDriveResult::Stopped,
                ResponseEnd::Failed(error) => {
                    return StreamDriveResult::FailedDuringStream { error };
                }
                ResponseEnd::Interrupted {
                    error,
                    transient,
                    overflow,
                } => {
                    let error = format!(
                        "Provider request {message_id} stream failed after {} ms: {error}",
                        started.elapsed().as_millis()
                    );
                    tracing::warn!(request_id = %message_id, provider_id = %provider_id, model_id = %model_id,
                        elapsed_ms = started.elapsed().as_millis(), error = %error,
                        completed_script = response.boundary.is_some(), "Provider stream failed");
                    if response.boundary.is_some() {
                        return response.finish();
                    }
                    let compaction = compaction.take_if(|_| overflow);
                    if compaction.is_none() {
                        if !transient || reissues >= MAX_STREAM_REISSUES {
                            return StreamDriveResult::FailedDuringStream { error };
                        }
                        reissues += 1;
                    }
                    if let Err(failure) = self
                        .restart(
                            Some(error),
                            compaction.as_ref(),
                            &mut provider_request,
                            &provider_id,
                            &model_id,
                        )
                        .await
                    {
                        return failure.during_stream();
                    }
                    response = ResponseState::default();
                }
            }
        }
    }

    async fn restart(
        &self,
        reason: Option<String>,
        compaction: Option<&ContextCompaction>,
        provider_request: &mut ProviderRequest,
        provider_id: &ProviderId,
        model_id: &ModelId,
    ) -> Result<(), RestartFailure> {
        self.ensure_turn_owned()
            .await
            .map_err(RestartFailure::Ownership)?;
        if let Some(compaction) = compaction {
            *provider_request = self
                .compact(compaction, provider_request, provider_id, model_id)
                .await
                .map_err(RestartFailure::Compaction)?;
        }
        if let Some(reason) = reason
            && !self.forward(StreamEvent::Restart { reason })
        {
            return Err(RestartFailure::Stopped);
        }
        Ok(())
    }

    async fn settle_calls(&self, response: &mut ResponseState) {
        let calls = std::mem::take(&mut response.calls);
        let Some((call, rewrites)) = super::merged_calls::merge_response_calls(calls) else {
            return;
        };
        let profile = self
            .agent_manager
            .script_turn_context(&self.session_id)
            .ok()
            .map(|turn| turn.profile);
        let call = super::bash_habits::rewrite_script_call(call, profile).await;
        response.boundary = Some(protocol_boundary(call, rewrites));
    }

    /// A runtime that lost the session to another instance must not issue model requests for it.
    async fn ensure_turn_owned(&self) -> Result<(), String> {
        match self.session_store.owns_turn(&self.session_id).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!(
                "Session {} is owned by another instance; not reissuing the model request",
                self.session_id
            )),
            Err(error) => Err(format!(
                "Could not confirm ownership of session {} before reissuing the model request: {error:#}",
                self.session_id
            )),
        }
    }

    async fn read_response(
        &self,
        mut stream: BoxStream<'static, color_eyre::Result<ProviderStreamEvent>>,
        provider_id: &ProviderId,
        response: &mut ResponseState,
    ) -> ResponseEnd {
        let mut drained_events = 0_usize;
        loop {
            let Some(event) = stream.next().await else {
                return ResponseEnd::Finished;
            };
            let draining = !response.calls.is_empty();
            if draining {
                drained_events = drained_events.saturating_add(1);
                if drained_events.is_multiple_of(POST_BOUNDARY_DRAIN_YIELD_INTERVAL) {
                    tokio::task::yield_now().await;
                }
            }
            let forwarded = match event {
                Err(error) => {
                    return ResponseEnd::Interrupted {
                        transient: kraai_provider_core::is_transient_error(&error),
                        overflow: kraai_provider_core::is_context_overflow_error(&error),
                        error: format!("{error:#}"),
                    };
                }
                Ok(ProviderStreamEvent::Compaction { .. }) => {
                    return ResponseEnd::Failed(
                        "Unexpected compaction item in a conversation response".into(),
                    );
                }
                Ok(ProviderStreamEvent::ReasoningDelta { item_id, delta }) => {
                    if draining {
                        continue;
                    }
                    self.forward(StreamEvent::ReasoningDelta { item_id, delta })
                }
                Ok(ProviderStreamEvent::Reasoning { payload }) => {
                    self.forward(StreamEvent::Reasoning {
                        provider_id: provider_id.clone(),
                        payload,
                    })
                }
                Ok(ProviderStreamEvent::TextDelta {
                    item_id,
                    phase,
                    delta,
                }) => {
                    if draining {
                        tracing::trace!(
                            discarded_bytes = delta.len(),
                            "Discarding text after script protocol boundary"
                        );
                        continue;
                    }
                    response.answered = true;
                    self.forward(StreamEvent::Text {
                        item_id,
                        phase,
                        delta,
                    })
                }
                Ok(ProviderStreamEvent::ScriptCall(call)) => {
                    if !draining
                        && call.arguments_error.is_none()
                        && call.name != ScriptToolDefinition::NAME
                    {
                        return ResponseEnd::Failed(format!(
                            "provider called unexpected tool '{}'",
                            call.name
                        ));
                    }
                    response.calls.push(call);
                    true
                }
                Ok(ProviderStreamEvent::Finished { stop_reason }) => {
                    response.reached_output_limit =
                        stop_reason == kraai_provider_core::StopReason::MaxOutputTokens;
                    true
                }
                Ok(ProviderStreamEvent::Usage(usage)) => {
                    if !self.forward(StreamEvent::Usage(usage)) {
                        return ResponseEnd::Stopped;
                    }
                    if draining {
                        tracing::debug!(
                            drained_events,
                            "Provider usage received after script protocol boundary"
                        );
                        return ResponseEnd::Finished;
                    }
                    true
                }
            };
            if !forwarded {
                return ResponseEnd::Stopped;
            }
        }
    }
}

#[derive(Default)]
struct ResponseState {
    calls: Vec<ScriptCall>,
    boundary: Option<CompletedProtocolBoundary>,
    reached_output_limit: bool,
    answered: bool,
}

impl ResponseState {
    fn ran_script(&self) -> bool {
        self.boundary
            .as_ref()
            .is_some_and(|boundary| boundary.protocol_error.is_none())
    }

    fn finish(self) -> StreamDriveResult {
        let (call, script, invalid_script, protocol_error, rewrites) = match self.boundary {
            Some(boundary) => (
                Some(boundary.call),
                boundary.script,
                boundary.invalid_script,
                boundary.protocol_error,
                boundary.rewrites,
            ),
            None => (None, None, None, None, ScriptRewrites::default()),
        };
        StreamDriveResult::Completed(Box::new(CompletedStreamOutput {
            call,
            script,
            invalid_script,
            protocol_error,
            rewrites,
            reached_output_limit: self.reached_output_limit,
        }))
    }
}

enum ResponseEnd {
    Finished,
    Stopped,
    Failed(String),
    Interrupted {
        error: String,
        transient: bool,
        overflow: bool,
    },
}

enum RestartFailure {
    Ownership(String),
    Compaction(String),
    Stopped,
}

impl RestartFailure {
    fn during_stream(self) -> StreamDriveResult {
        match self {
            Self::Ownership(error) | Self::Compaction(error) => {
                StreamDriveResult::FailedDuringStream { error }
            }
            Self::Stopped => StreamDriveResult::Stopped,
        }
    }
}

fn output_limit_notice(limit: Option<usize>) -> String {
    let limit = limit.map_or_else(String::new, |tokens| format!(" of {tokens} tokens"));
    format!(
        "Your previous response reached the output limit{limit} before its script call finished, so it was discarded. Respond again within that limit, and split a large file edit into several smaller kraai-edit-file calls."
    )
}

fn protocol_boundary(call: ScriptCall, mut rewrites: ScriptRewrites) -> CompletedProtocolBoundary {
    if let Some(rewrite) = &call.rewrite {
        rewrites.bash.clone_from(&rewrite.changes);
    }
    let parsed = match &call.arguments_error {
        Some(error) => Err(ProtocolError::InvalidToolCall(error.clone())),
        None => parse_script_input(&call.input),
    };
    match parsed {
        Ok(script) => CompletedProtocolBoundary {
            call,
            script: Some(script),
            invalid_script: None,
            protocol_error: None,
            rewrites,
        },
        Err(error) => CompletedProtocolBoundary {
            script: None,
            invalid_script: Some(InvalidScriptBlock {
                input: call.input.clone(),
                source: call.input.as_bytes().to_vec(),
                timeout: None,
                requested_capabilities: SandboxCapabilities::default(),
            }),
            protocol_error: Some(error),
            rewrites,
            call,
        },
    }
}

#[cfg(debug_assertions)]
fn log_file_contexts(session_id: &SessionId, message_id: &MessageId, request: &ProviderRequest) {
    for (position, item) in request.messages.iter().enumerate() {
        if let ConversationItem::FileContext { text } = item {
            tracing::info!(
                session_id = %session_id,
                request_id = %message_id,
                position,
                bytes = text.len(),
                "Provider request file context"
            );
        }
    }
}

#[cfg(not(debug_assertions))]
fn log_file_contexts(_: &SessionId, _: &MessageId, _: &ProviderRequest) {}
