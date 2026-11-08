use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream::BoxStream;
use kraai_agent::{ContextCompaction, PendingStreamRequest};
use kraai_execution_types::SandboxCapabilities;
use kraai_provider_core::{
    ProviderManager, ProviderRequest, ProviderRequestContext, ProviderStreamEvent,
    ScriptToolDefinition,
};
use kraai_script_protocol::{InvalidScriptBlock, ProtocolError, ScriptBlock, parse_script_input};
use kraai_types::{
    ConversationItem, MessageId, ModelId, ProviderId, ScriptCall, SessionId, ToolCallId,
};
use tokio::sync::{RwLock, mpsc};

use super::core::emit_event;
use super::request_usage::RuntimeRetryObserver;
use super::session::{SessionMessage, StreamEvent};
use crate::api::Event;
use crate::handle::RuntimeEventSender;

const POST_BOUNDARY_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STREAM_REISSUES: u32 = 2;
pub(super) const POST_BOUNDARY_DRAIN_YIELD_INTERVAL: usize = 256;

struct CompletedProtocolBoundary {
    call_id: ToolCallId,
    script: Option<ScriptBlock>,
    invalid_script: Option<InvalidScriptBlock>,
    protocol_error: Option<ProtocolError>,
    /// What Kraai changed in the model's script, for the notice in its result.
    rewrites: Vec<String>,
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
    pub(super) call_id: Option<ToolCallId>,
    pub(super) script: Option<ScriptBlock>,
    pub(super) invalid_script: Option<InvalidScriptBlock>,
    pub(super) protocol_error: Option<ProtocolError>,
    pub(super) rewrites: Vec<String>,
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

/// Reads one provider response and forwards its events to the session actor, which applies
/// them. The reader never mutates the streaming message itself.
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
    /// Used once if the provider reports that the request overflowed the context window.
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
                .observe_usage(self.barrier.clone(), on_auxiliary_usage.clone())
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
        self.warm_cache(
            &provider_id,
            &model_id,
            &provider_request,
            on_auxiliary_usage,
        )
        .await;
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

    /// Returns false once the actor no longer accepts this stream's events.
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

    async fn warm_cache(
        &self,
        provider_id: &ProviderId,
        model_id: &ModelId,
        provider_request: &ProviderRequest,
        on_auxiliary_usage: Arc<dyn Fn(kraai_types::RequestUsage) + Send + Sync>,
    ) {
        let warmup = match self.providers.prepare_cache_warmup(
            provider_id,
            model_id,
            self.session_id.as_str(),
            provider_request,
        ) {
            Ok(Some(warmup)) => warmup,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "Cache warming failed; continuing with the conversation");
                return;
            }
        };
        let recorder = kraai_agent::AuxiliaryUsageRecorder {
            store: self.agent_manager.request_usage_store(),
            session_id: self.session_id.clone(),
            barrier: Some(self.barrier.clone()),
            on_usage: Some(on_auxiliary_usage),
        };
        if let Err(error) = super::cache_warming::warm_cache(
            &self.providers,
            provider_id,
            model_id,
            warmup,
            recorder,
            self.image_resolver(),
        )
        .await
        {
            tracing::warn!(error = %format!("{error:#}"), "Cache warming failed; continuing with the conversation");
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
                        if let Err(error) = self.ensure_turn_owned().await {
                            return StreamDriveResult::FailedToStart { error };
                        }
                        match self
                            .compact(&compaction, &provider_request, &provider_id, &model_id)
                            .await
                        {
                            Ok(compacted) => {
                                provider_request = compacted;
                                continue;
                            }
                            Err(error) => return StreamDriveResult::FailedToStart { error },
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
            match self
                .read_response(stream, &provider_id, &mut response)
                .await
            {
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
                    let recovery =
                        match compaction.take_if(|_| context_bound) {
                            Some(compaction) => OutputLimitRecovery::Compact(Box::new(compaction)),
                            None if !warned_about_output_limit
                                && response.boundary.is_none()
                                && !response.answered =>
                            {
                                OutputLimitRecovery::Warn(model.and_then(|model| {
                                    model.output_limit(&provider_request.messages)
                                }))
                            }
                            None => return response.finish(),
                        };
                    if let Err(error) = self.ensure_turn_owned().await {
                        return StreamDriveResult::FailedDuringStream { error };
                    }
                    match recovery {
                        OutputLimitRecovery::Compact(compaction) => match self
                            .compact(&compaction, &provider_request, &provider_id, &model_id)
                            .await
                        {
                            Ok(compacted) => provider_request = compacted,
                            Err(error) => {
                                tracing::warn!(%error, "Compaction after a cut off response failed; keeping the response");
                                return response.finish();
                            }
                        },
                        OutputLimitRecovery::Warn(limit) => {
                            warned_about_output_limit = true;
                            provider_request
                                .messages
                                .push(ConversationItem::FileContext {
                                    text: output_limit_notice(limit),
                                });
                        }
                    }
                    if !self.forward(StreamEvent::Restart {
                        reason: String::from("The response reached the output token limit"),
                    }) {
                        return StreamDriveResult::Stopped;
                    }
                    response = ResponseState::default();
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
                    if overflow && let Some(compaction) = compaction.take() {
                        if let Err(error) = self.ensure_turn_owned().await {
                            return StreamDriveResult::FailedDuringStream { error };
                        }
                        if !self.forward(StreamEvent::Restart { reason: error }) {
                            return StreamDriveResult::Stopped;
                        }
                        response = ResponseState::default();
                        match self
                            .compact(&compaction, &provider_request, &provider_id, &model_id)
                            .await
                        {
                            Ok(compacted) => provider_request = compacted,
                            Err(error) => return StreamDriveResult::FailedDuringStream { error },
                        }
                        continue;
                    }
                    if !transient || reissues >= MAX_STREAM_REISSUES {
                        return StreamDriveResult::FailedDuringStream { error };
                    }
                    if let Err(error) = self.ensure_turn_owned().await {
                        return StreamDriveResult::FailedDuringStream { error };
                    }
                    reissues += 1;
                    response = ResponseState::default();
                    if !self.forward(StreamEvent::Restart { reason: error }) {
                        return StreamDriveResult::Stopped;
                    }
                }
            }
        }
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
        let mut drain_deadline = None;
        let mut drained_events = 0_usize;
        loop {
            let next = match drain_deadline {
                Some(deadline) => match tokio::time::timeout_at(deadline, stream.next()).await {
                    Ok(event) => event,
                    Err(_) => {
                        tracing::warn!(
                            timeout_millis = POST_BOUNDARY_DRAIN_TIMEOUT.as_millis(),
                            "Stopping provider stream drain after timeout"
                        );
                        return ResponseEnd::Finished;
                    }
                },
                None => stream.next().await,
            };
            let Some(event) = next else {
                return ResponseEnd::Finished;
            };
            let draining = response.boundary.is_some();
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
                    if draining {
                        // Only the first call runs; later ones stay in the response and get a
                        // result saying they did not run.
                        tracing::debug!(call_id = %call.call_id, "Keeping a script call after the first without running it");
                        if !self.forward(StreamEvent::ScriptCall(call)) {
                            return ResponseEnd::Stopped;
                        }
                        continue;
                    }
                    if call.arguments_error.is_none() && call.name != ScriptToolDefinition::NAME {
                        return ResponseEnd::Failed(format!(
                            "provider called unexpected tool '{}'",
                            call.name
                        ));
                    }
                    let profile = self
                        .agent_manager
                        .script_turn_context(&self.session_id)
                        .ok()
                        .map(|turn| turn.profile);
                    let call = super::bash_habits::rewrite_script_call(call, profile).await;
                    response.boundary = Some(protocol_boundary(&call));
                    drain_deadline =
                        Some(tokio::time::Instant::now() + POST_BOUNDARY_DRAIN_TIMEOUT);
                    self.forward(StreamEvent::ScriptCall(call))
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
    boundary: Option<CompletedProtocolBoundary>,
    reached_output_limit: bool,
    /// The response streamed text, so a cut off answer is visible to the user.
    answered: bool,
}

impl ResponseState {
    fn ran_script(&self) -> bool {
        self.boundary
            .as_ref()
            .is_some_and(|boundary| boundary.protocol_error.is_none())
    }

    fn finish(self) -> StreamDriveResult {
        let (call_id, script, invalid_script, protocol_error, rewrites) = match self.boundary {
            Some(boundary) => (
                Some(boundary.call_id),
                boundary.script,
                boundary.invalid_script,
                boundary.protocol_error,
                boundary.rewrites,
            ),
            None => (None, None, None, None, Vec::new()),
        };
        StreamDriveResult::Completed(Box::new(CompletedStreamOutput {
            call_id,
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
        /// The request did not fit the model's context window.
        overflow: bool,
    },
}

enum OutputLimitRecovery {
    Compact(Box<ContextCompaction>),
    /// Retry once, telling the model its last response was cut off at this many tokens.
    Warn(Option<usize>),
}

fn output_limit_notice(limit: Option<usize>) -> String {
    let limit = limit.map_or_else(String::new, |tokens| format!(" of {tokens} tokens"));
    format!(
        "Your previous response reached the output limit{limit} before its script call finished, so it was discarded. Respond again within that limit, and split a large file edit into several smaller kraai-edit-file calls."
    )
}

fn protocol_boundary(call: &ScriptCall) -> CompletedProtocolBoundary {
    let call_id = call.call_id.clone();
    let input = call.input.as_str();
    let rewrites = call
        .rewrite
        .as_ref()
        .map(|rewrite| rewrite.changes.clone())
        .unwrap_or_default();
    let parsed = match &call.arguments_error {
        Some(error) => Err(ProtocolError::InvalidToolCall(error.clone())),
        None => parse_script_input(input),
    };
    match parsed {
        Ok(script) => CompletedProtocolBoundary {
            call_id,
            script: Some(script),
            invalid_script: None,
            protocol_error: None,
            rewrites,
        },
        Err(error) => CompletedProtocolBoundary {
            call_id,
            script: None,
            invalid_script: Some(InvalidScriptBlock {
                input: input.to_string(),
                source: input.as_bytes().to_vec(),
                timeout: None,
                requested_capabilities: SandboxCapabilities::default(),
            }),
            protocol_error: Some(error),
            rewrites,
        },
    }
}

#[cfg(debug_assertions)]
fn log_file_contexts(session_id: &SessionId, message_id: &MessageId, request: &ProviderRequest) {
    for (position, item) in request.messages.iter().enumerate() {
        if let kraai_types::ConversationItem::FileContext { text } = item {
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
