use kraai_agent::{ToolExecutionPayload, ToolExecutionRequest};
use kraai_tool_core::{ToolContext, ToolOutput};
use kraai_types::{ModelId, ProviderId};

use super::core::{QueuedMessage, RuntimeCore, emit_event};
use super::streaming::StreamJobKind;
use crate::api::Event;
use crate::handle::Command;

async fn execute_tool_requests(
    executions: Vec<ToolExecutionRequest>,
) -> Vec<kraai_types::ToolResult> {
    let mut results = Vec::with_capacity(executions.len());

    for execution in executions {
        let (output, permission_denied, tool_state_deltas) = match execution.payload {
            ToolExecutionPayload::Denied => (
                serde_json::json!({ "error": "Permission denied by user" }),
                true,
                Vec::new(),
            ),
            ToolExecutionPayload::Approved {
                prepared,
                config,
                tool_state_snapshot,
            } => {
                let ctx = ToolContext {
                    global_config: &config,
                    tool_state_snapshot: &tool_state_snapshot,
                };
                let result = prepared.call(&ctx).await;
                match result.output {
                    ToolOutput::Success { data } => (data, false, result.tool_state_deltas),
                    ToolOutput::Error { message } => {
                        (serde_json::json!({ "error": message }), false, Vec::new())
                    }
                }
            }
        };

        results.push(kraai_types::ToolResult {
            call_id: execution.call_id,
            tool_id: execution.tool_id,
            output,
            permission_denied,
            tool_state_deltas,
        });
    }

    results
}

impl RuntimeCore {
    pub(crate) async fn handle_send_message(
        &self,
        session_id: String,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
        auto_approve: bool,
    ) {
        let has_queued_messages = {
            let queued = self.queued_messages.lock().await;
            queued
                .get(&session_id)
                .is_some_and(|queue| !queue.is_empty())
        };
        let is_turn_active = {
            let agent = self.agent_manager.lock().await;
            agent.is_turn_active(&session_id)
        };
        if is_turn_active || has_queued_messages {
            self.enqueue_message(
                &session_id,
                QueuedMessage {
                    message,
                    model_id,
                    provider_id,
                    auto_approve,
                },
            )
            .await;
            self.schedule_queue_drain(&session_id).await;
            return;
        }

        let stream_request = {
            let mut agent = self.agent_manager.lock().await;
            match agent
                .prepare_start_stream_with_options(
                    &session_id,
                    message,
                    model_id,
                    provider_id,
                    auto_approve,
                )
                .await
            {
                Ok(result) => Some((agent.cloned_provider_manager(), result)),
                Err(error) => {
                    self.send_event(Event::Error(error.to_string()));
                    None
                }
            }
        };

        let Some((providers, request)) = stream_request else {
            self.schedule_queue_drain(&session_id).await;
            return;
        };

        self.start_stream_job(StreamJobKind::Initial, session_id, providers, request)
            .await;
    }

    async fn enqueue_message(&self, session_id: &str, queued_message: QueuedMessage) {
        let mut queued = self.queued_messages.lock().await;
        queued
            .entry(session_id.to_string())
            .or_default()
            .push_back(queued_message);
    }

    pub(crate) async fn handle_start_queued_messages(&self, session_id: String) {
        let is_turn_active = {
            let agent = self.agent_manager.lock().await;
            agent.is_turn_active(&session_id)
        };
        if is_turn_active {
            return;
        }

        loop {
            let next_message = {
                let mut queued = self.queued_messages.lock().await;
                let Some(queue) = queued.get_mut(&session_id) else {
                    return;
                };
                let next = queue.pop_front();
                if queue.is_empty() {
                    queued.remove(&session_id);
                }
                next
            };

            let Some(next_message) = next_message else {
                return;
            };

            let stream_request = {
                let mut agent = self.agent_manager.lock().await;
                match agent
                    .prepare_start_stream_with_options(
                        &session_id,
                        next_message.message,
                        next_message.model_id,
                        next_message.provider_id,
                        next_message.auto_approve,
                    )
                    .await
                {
                    Ok(result) => Some((agent.cloned_provider_manager(), result)),
                    Err(error) => {
                        self.send_event(Event::Error(error.to_string()));
                        None
                    }
                }
            };

            let Some((providers, request)) = stream_request else {
                continue;
            };

            self.start_stream_job(StreamJobKind::Initial, session_id, providers, request)
                .await;
            return;
        }
    }

    pub(crate) async fn schedule_queue_drain(&self, session_id: &str) {
        let _ = self
            .command_tx
            .send(Command::StartQueuedMessages {
                session_id: session_id.to_string(),
            })
            .await;
    }

    pub(crate) async fn handle_execute_tools(&self, session_id: String) {
        let runtime = self.clone();

        tokio::spawn(async move {
            let executions = {
                let mut agent = runtime.agent_manager.lock().await;
                agent.take_ready_tool_executions(&session_id)
            };
            let executed_source_message_ids: Vec<_> = executions
                .iter()
                .map(|execution| execution.source_message_id.clone())
                .collect();
            let mut completed_source_message_ids = Vec::new();
            for execution in &executions {
                if !completed_source_message_ids.contains(&execution.source_message_id) {
                    completed_source_message_ids.push(execution.source_message_id.clone());
                }
            }

            let results = execute_tool_requests(executions).await;

            for result in &results {
                let success = result.output.get("error").is_none();
                let output = serde_json::to_string(&result.output).unwrap_or_default();

                emit_event(
                    &runtime.event_tx,
                    Event::ToolResultReady {
                        session_id: session_id.clone(),
                        call_id: result.call_id.to_string(),
                        tool_id: result.tool_id.to_string(),
                        success,
                        output,
                        denied: result.permission_denied,
                    },
                );
            }

            {
                let mut agent = runtime.agent_manager.lock().await;
                if let Err(error) = agent
                    .add_tool_results_to_history(&session_id, results)
                    .await
                {
                    agent.clear_active_turn(&session_id);
                    drop(agent);
                    emit_event(&runtime.event_tx, Event::Error(error.to_string()));
                    emit_event(
                        &runtime.event_tx,
                        Event::ContinuationFailed {
                            session_id: session_id.clone(),
                            error: error.to_string(),
                        },
                    );
                    emit_event(
                        &runtime.event_tx,
                        Event::HistoryUpdated {
                            session_id: session_id.clone(),
                        },
                    );
                    runtime.schedule_queue_drain(&session_id).await;
                    return;
                }
                agent.finish_tool_executions(&session_id, &executed_source_message_ids);
            }

            tracing::debug!("Emitting HistoryUpdated event after tool results");
            emit_event(
                &runtime.event_tx,
                Event::HistoryUpdated {
                    session_id: session_id.clone(),
                },
            );

            for source_message_id in completed_source_message_ids {
                let has_pending_tools = {
                    runtime
                        .agent_manager
                        .lock()
                        .await
                        .has_unfinished_tools_for_message(&session_id, &source_message_id)
                };
                if has_pending_tools {
                    continue;
                }

                runtime.spawn_continuation(session_id.clone());
            }
        });
    }

    pub(crate) async fn process_completed_stream_output(
        &self,
        completed_session: String,
        source_message_id: kraai_types::MessageId,
        content: String,
    ) {
        let (tool_calls, failed) = {
            let mut agent = self.agent_manager.lock().await;
            match agent
                .parse_tool_calls_from_content(&completed_session, &source_message_id, &content)
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    {
                        let mut agent = self.agent_manager.lock().await;
                        agent.clear_active_turn(&completed_session);
                    }
                    self.schedule_queue_drain(&completed_session).await;
                    emit_event(
                        &self.event_tx,
                        Event::HistoryUpdated {
                            session_id: completed_session,
                        },
                    );
                    emit_event(&self.event_tx, Event::Error(error.to_string()));
                    return;
                }
            }
        };

        tracing::debug!(
            "Found {} tool calls, {} failed",
            tool_calls.len(),
            failed.len()
        );

        if !failed.is_empty() {
            tracing::warn!("Failed tool calls found, adding to history");
            let add_result = {
                let mut agent = self.agent_manager.lock().await;
                agent
                    .add_parse_failures_to_history(&completed_session, failed)
                    .await
            };
            if let Err(error) = add_result {
                {
                    let mut agent = self.agent_manager.lock().await;
                    agent.clear_active_turn(&completed_session);
                }
                self.schedule_queue_drain(&completed_session).await;
                emit_event(&self.event_tx, Event::Error(error.to_string()));
                emit_event(
                    &self.event_tx,
                    Event::ContinuationFailed {
                        session_id: completed_session,
                        error: error.to_string(),
                    },
                );
                return;
            }
            emit_event(
                &self.event_tx,
                Event::HistoryUpdated {
                    session_id: completed_session.clone(),
                },
            );
            self.spawn_continuation(completed_session);
            return;
        }

        let had_tool_calls = !tool_calls.is_empty();
        let mut has_auto_approved_tools = false;

        for tool_call in tool_calls {
            let args_json = {
                let agent = self.agent_manager.lock().await;
                agent
                    .get_pending_tool_args(&completed_session, &tool_call.call_id)
                    .map(|args| serde_json::to_string(&args).unwrap_or_default())
                    .unwrap_or_default()
            };

            if tool_call.requires_confirmation {
                tracing::debug!(
                    "Emitting ToolCallDetected: {} - {}",
                    tool_call.tool_id,
                    tool_call.description
                );
                emit_event(
                    &self.event_tx,
                    Event::ToolCallDetected {
                        session_id: completed_session.clone(),
                        call_id: tool_call.call_id.to_string(),
                        tool_id: tool_call.tool_id,
                        args: args_json,
                        description: tool_call.description,
                        risk_level: tool_call.assessment.risk.as_str().to_string(),
                        reasons: tool_call.assessment.reasons,
                        queue_order: tool_call.queue_order,
                    },
                );
            } else {
                has_auto_approved_tools = true;
            }
        }

        if has_auto_approved_tools {
            let _ = self
                .command_tx
                .send(Command::ExecuteApprovedTools {
                    session_id: completed_session,
                })
                .await;
        } else if !had_tool_calls {
            {
                let mut agent = self.agent_manager.lock().await;
                agent.clear_active_turn(&completed_session);
            }
            self.schedule_queue_drain(&completed_session).await;
            emit_event(
                &self.event_tx,
                Event::HistoryUpdated {
                    session_id: completed_session,
                },
            );
        }
    }
}
