use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use kraai_agent::PendingStreamRequest;
use kraai_provider_core::{
    ProviderManager, ProviderRequestContext, ProviderStreamEvent, ScriptToolTransport,
};
use kraai_script_protocol::{
    InvalidScriptBlock, ProtocolError, ScriptBlock, ScriptProtocolParser, parse_script_input,
};
use kraai_types::{SandboxCapabilities, ToolCallId};
use ulid::Ulid;

use super::core::{RuntimeCore, emit_event};
use super::request_usage::RuntimeRetryObserver;
use crate::api::Event;
use crate::handle::RuntimeEventSender;

const POST_BOUNDARY_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const POST_BOUNDARY_DRAIN_YIELD_INTERVAL: usize = 256;

struct CompletedProtocolBoundary {
    call_id: ToolCallId,
    script: Option<ScriptBlock>,
    invalid_script: Option<InvalidScriptBlock>,
    protocol_error: Option<ProtocolError>,
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
    pub(super) session_id: String,
    pub(super) call_id: Option<ToolCallId>,
    pub(super) script: Option<ScriptBlock>,
    pub(super) invalid_script: Option<InvalidScriptBlock>,
    pub(super) protocol_error: Option<ProtocolError>,
}

impl RuntimeCore {
    pub(super) async fn drive_stream(
        session_id: String,
        request: PendingStreamRequest,
        providers: ProviderManager,
        agent_manager: Arc<tokio::sync::RwLock<kraai_agent::AgentManager>>,
        event_tx: RuntimeEventSender,
        session_state_barrier: Arc<tokio::sync::RwLock<()>>,
    ) -> StreamDriveResult {
        let PendingStreamRequest {
            message_id,
            provider_id,
            model_id,
            mut provider_request,
            script_tool_transport,
            context_notifications: _,
            context_compaction,
        } = request;
        let usage_event_tx = event_tx.clone();
        let usage_session_id = session_id.clone();
        let on_auxiliary_usage: Arc<dyn Fn(kraai_types::RequestUsage) + Send + Sync> =
            Arc::new(move |request| {
                emit_event(
                    &usage_event_tx,
                    Event::RequestUsageUpdated {
                        session_id: usage_session_id.clone(),
                        request: Box::new(request),
                    },
                );
            });
        if let Some(compaction) = context_compaction {
            let compaction = compaction
                .observe_usage(session_state_barrier.clone(), on_auxiliary_usage.clone())
                .with_image_resolver(Arc::new(super::images::StoredImageResolver(
                    agent_manager.read().await.image_store(),
                )));
            emit_event(
                &event_tx,
                Event::ContextStateChanged {
                    session_id: session_id.clone(),
                    notifications: vec![String::from("Compacting conversation context.")],
                },
            );
            let result = compaction.run(&providers, &provider_id, &model_id).await;
            match result {
                Ok(outcome) => {
                    provider_request = outcome.request;
                    emit_event(
                        &event_tx,
                        Event::ContextStateChanged {
                            session_id: session_id.clone(),
                            notifications: vec![outcome.notification],
                        },
                    );
                }
                Err(error) => {
                    emit_event(
                        &event_tx,
                        Event::ContextStateChanged {
                            session_id: session_id.clone(),
                            notifications: vec![String::from(
                                "Conversation context compaction failed.",
                            )],
                        },
                    );
                    return StreamDriveResult::FailedToStart {
                        error: format!("{error:#}"),
                    };
                }
            }
        }
        let warmup = match providers.prepare_cache_warmup(
            &provider_id,
            &model_id,
            &session_id,
            &provider_request,
        ) {
            Ok(warmup) => warmup,
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "Cache warming failed; continuing with the conversation");
                None
            }
        };
        if let Some(warmup) = warmup {
            let store = agent_manager.read().await.request_usage_store();
            let recorder = kraai_agent::AuxiliaryUsageRecorder {
                store,
                session_id: session_id.clone(),
                barrier: Some(session_state_barrier.clone()),
                on_usage: Some(on_auxiliary_usage),
            };
            if let Err(error) = super::cache_warming::warm_cache(
                &providers,
                &provider_id,
                &model_id,
                warmup,
                recorder,
                Arc::new(super::images::StoredImageResolver(
                    agent_manager.read().await.image_store(),
                )),
            )
            .await
            {
                tracing::warn!(error = %format!("{error:#}"), "Cache warming failed; continuing with the conversation");
            }
        }
        let request_context = ProviderRequestContext::with_retry_observer_and_prompt_cache_key(
            Arc::new(RuntimeRetryObserver {
                session_id: session_id.clone(),
                provider_id: provider_id.clone(),
                model_id: model_id.clone(),
                event_tx: event_tx.clone(),
                message_id: message_id.clone(),
                agent_manager: Arc::clone(&agent_manager),
                session_state_barrier: Arc::clone(&session_state_barrier),
            }),
            session_id.clone(),
        )
        .with_image_resolver(Arc::new(super::images::StoredImageResolver(
            agent_manager.read().await.image_store(),
        )));
        {
            let _state_guard = session_state_barrier.read().await;
            match agent_manager
                .read()
                .await
                .record_request_started(&message_id)
                .await
            {
                Ok(Some(request)) => emit_event(
                    &event_tx,
                    Event::RequestUsageUpdated {
                        session_id: session_id.clone(),
                        request: Box::new(request),
                    },
                ),
                Ok(None) => {}
                Err(error) => {
                    return StreamDriveResult::FailedToStart {
                        error: format!("{error:#}"),
                    };
                }
            }
        }
        let request_started = Instant::now();
        let mut stream = match providers
            .generate_reply_stream(
                provider_id.clone(),
                &model_id,
                provider_request,
                request_context,
            )
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                let error = format!(
                    "Provider request {message_id} failed to start after {} ms: {error:#}",
                    request_started.elapsed().as_millis()
                );
                tracing::error!(request_id = %message_id, provider_id = %provider_id, model_id = %model_id,
                    elapsed_ms = request_started.elapsed().as_millis(), error = %error,
                    "Provider request failed to start");
                return StreamDriveResult::FailedToStart { error };
            }
        };

        let mut parser = ScriptProtocolParser::new();
        let mut completed_boundary = None;
        let mut post_boundary_deadline = None;
        let mut post_boundary_events = 0_usize;
        let mut last_text_item = None;

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
                Ok(ProviderStreamEvent::Compaction { .. }) => {
                    return StreamDriveResult::FailedDuringStream {
                        error: "Unexpected compaction item in a conversation response".into(),
                    };
                }
                Ok(ProviderStreamEvent::Reasoning { payload }) => {
                    let _state_guard = session_state_barrier.read().await;
                    if agent_manager
                        .read()
                        .await
                        .append_reasoning(&message_id, provider_id.clone(), payload)
                        .await
                        .is_none()
                    {
                        return StreamDriveResult::Stopped;
                    }
                }
                Ok(ProviderStreamEvent::TextDelta {
                    item_id,
                    phase,
                    delta,
                }) => {
                    if completed_boundary.is_some() {
                        tracing::trace!(
                            discarded_bytes = delta.len(),
                            "Discarding text after script protocol boundary"
                        );
                        continue;
                    }
                    if script_tool_transport == ScriptToolTransport::NativeCustom {
                        let _state_guard = session_state_barrier.read().await;
                        let visible = {
                            let agent = agent_manager.read().await;
                            agent
                                .append_text_chunk(&message_id, &item_id, phase, &delta)
                                .await
                        };
                        let Some(visible) = visible else {
                            return StreamDriveResult::Stopped;
                        };
                        if !visible.is_empty() {
                            emit_event(
                                &event_tx,
                                Event::StreamChunk {
                                    session_id: session_id.clone(),
                                    message_id: message_id.to_string(),
                                    chunk: visible,
                                },
                            );
                        }
                        continue;
                    }

                    last_text_item = Some((item_id.clone(), phase));

                    let parsed = parser.ingest(&delta);
                    if !parsed.accepted.is_empty() {
                        let _state_guard = session_state_barrier.read().await;
                        let visible = {
                            let agent = agent_manager.read().await;
                            agent
                                .append_text_chunk(&message_id, &item_id, phase, &parsed.accepted)
                                .await
                        };
                        let Some(visible) = visible else {
                            return StreamDriveResult::Stopped;
                        };
                        emit_event(
                            &event_tx,
                            Event::StreamChunk {
                                session_id: session_id.clone(),
                                message_id: message_id.to_string(),
                                chunk: visible,
                            },
                        );
                    }
                    if parsed.should_stop {
                        let invalid_script = parsed.error.as_ref().map(|_| parser.invalid_block());
                        let input = parsed
                            .completed
                            .as_ref()
                            .map(|script| script.input.clone())
                            .or_else(|| {
                                invalid_script.as_ref().map(|invalid| invalid.input.clone())
                            })
                            .unwrap_or_default();
                        let call_id = ToolCallId::new(format!("kraai-{}", Ulid::generate()));
                        let _state_guard = session_state_barrier.read().await;
                        let visible = {
                            let agent = agent_manager.read().await;
                            agent
                                .append_script_call(
                                    &message_id,
                                    call_id.clone(),
                                    String::from("kraai_nushell"),
                                    input,
                                )
                                .await
                        };
                        let Some(visible) = visible else {
                            return StreamDriveResult::Stopped;
                        };
                        emit_event(
                            &event_tx,
                            Event::StreamChunk {
                                session_id: session_id.clone(),
                                message_id: message_id.to_string(),
                                chunk: visible,
                            },
                        );
                        tracing::debug!(
                            "Script protocol boundary reached; draining provider stream for usage"
                        );
                        completed_boundary = Some(CompletedProtocolBoundary {
                            call_id,
                            script: parsed.completed,
                            invalid_script,
                            protocol_error: parsed.error,
                        });
                        post_boundary_deadline =
                            Some(tokio::time::Instant::now() + POST_BOUNDARY_DRAIN_TIMEOUT);
                    }
                }
                Ok(ProviderStreamEvent::ScriptCall {
                    call_id,
                    name,
                    input,
                }) => {
                    if completed_boundary.is_some() {
                        return StreamDriveResult::FailedDuringStream {
                            error: String::from(
                                "provider emitted more than one script call in one response",
                            ),
                        };
                    }
                    if script_tool_transport != ScriptToolTransport::NativeCustom {
                        return StreamDriveResult::FailedDuringStream {
                            error: String::from(
                                "text-envelope provider emitted an unexpected native script call",
                            ),
                        };
                    }
                    if name != "kraai_nushell" {
                        return StreamDriveResult::FailedDuringStream {
                            error: format!("provider called unexpected custom tool '{name}'"),
                        };
                    }

                    let (script, invalid_script, protocol_error) = match parse_script_input(&input)
                    {
                        Ok(script) => (Some(script), None, None),
                        Err(error) => (
                            None,
                            Some(InvalidScriptBlock {
                                input: input.clone(),
                                source: input.as_bytes().to_vec(),
                                timeout: None,
                                requested_capabilities: SandboxCapabilities::default(),
                            }),
                            Some(error),
                        ),
                    };
                    let _state_guard = session_state_barrier.read().await;
                    let visible = {
                        let agent = agent_manager.read().await;
                        agent
                            .append_script_call(&message_id, call_id.clone(), name, input)
                            .await
                    };
                    let Some(visible) = visible else {
                        return StreamDriveResult::Stopped;
                    };
                    emit_event(
                        &event_tx,
                        Event::StreamChunk {
                            session_id: session_id.clone(),
                            message_id: message_id.to_string(),
                            chunk: visible,
                        },
                    );
                    completed_boundary = Some(CompletedProtocolBoundary {
                        call_id,
                        script,
                        invalid_script,
                        protocol_error,
                    });
                    post_boundary_deadline =
                        Some(tokio::time::Instant::now() + POST_BOUNDARY_DRAIN_TIMEOUT);
                }
                Ok(ProviderStreamEvent::Usage(usage)) => {
                    let _state_guard = session_state_barrier.read().await;
                    let agent = agent_manager.read().await;
                    match agent.set_streaming_message_usage(&message_id, usage).await {
                        Ok(Some(request)) => emit_event(
                            &event_tx,
                            Event::RequestUsageUpdated {
                                session_id: session_id.clone(),
                                request: Box::new(request),
                            },
                        ),
                        Ok(None) => return StreamDriveResult::Stopped,
                        Err(error) => {
                            return StreamDriveResult::FailedDuringStream {
                                error: format!("{error:#}"),
                            };
                        }
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
                    let error = format!(
                        "Provider request {message_id} stream failed after {} ms: {error:#}",
                        request_started.elapsed().as_millis()
                    );
                    tracing::warn!(request_id = %message_id, provider_id = %provider_id, model_id = %model_id,
                        elapsed_ms = request_started.elapsed().as_millis(), error = %error,
                        completed_script = completed_boundary.is_some(), "Provider stream failed");
                    if completed_boundary.is_some() {
                        break;
                    }
                    return StreamDriveResult::FailedDuringStream { error };
                }
            }
        }

        if let Some(boundary) = completed_boundary {
            tracing::debug!(
                drained_events = post_boundary_events,
                "Provider stream drain finished after script protocol boundary"
            );
            return StreamDriveResult::Completed(Box::new(CompletedStreamOutput {
                session_id,
                call_id: Some(boundary.call_id),
                script: boundary.script,
                invalid_script: boundary.invalid_script,
                protocol_error: boundary.protocol_error,
            }));
        }

        let tail = if script_tool_transport == ScriptToolTransport::TextEnvelope {
            parser.finish()
        } else {
            Default::default()
        };
        if !tail.accepted.is_empty() {
            let (item_id, phase) = last_text_item.unwrap_or_else(|| {
                (
                    String::from("text-envelope-message"),
                    kraai_types::AssistantPhase::FinalAnswer,
                )
            });
            let _state_guard = session_state_barrier.read().await;
            let visible = {
                let agent = agent_manager.read().await;
                agent
                    .append_text_chunk(&message_id, &item_id, phase, &tail.accepted)
                    .await
            };
            let Some(visible) = visible else {
                return StreamDriveResult::Stopped;
            };
            emit_event(
                &event_tx,
                Event::StreamChunk {
                    session_id: session_id.clone(),
                    message_id: message_id.to_string(),
                    chunk: visible,
                },
            );
        }

        let invalid_script = tail.error.as_ref().map(|_| parser.invalid_block());
        StreamDriveResult::Completed(Box::new(CompletedStreamOutput {
            session_id,
            call_id: None,
            script: tail.completed,
            invalid_script,
            protocol_error: tail.error,
        }))
    }
}
