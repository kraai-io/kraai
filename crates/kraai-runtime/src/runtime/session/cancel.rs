use color_eyre::eyre::{Result, eyre};
use kraai_types::{DomainError, ScriptExecutionStatus};

use super::actor::{CancelRequest, Responder, SessionActor, StreamJob, TurnState};
use crate::RuntimeError;
use crate::api::Event;

impl SessionActor {
    pub(in crate::runtime) async fn cancel(
        &mut self,
        discard_queue: bool,
        expected_lease: Option<i64>,
        response: Option<Responder<bool>>,
    ) {
        if let Some(expected) = expected_lease
            && !self
                .runtime
                .session_store
                .lease_token_matches(&self.session_id, expected)
        {
            answer(response, Ok(false));
            return;
        }
        let discarded = match self.discard_queue(discard_queue).await {
            Ok(discarded) => discarded,
            Err(error) => {
                answer(response, Err(error));
                return;
            }
        };
        match std::mem::replace(&mut self.state, TurnState::Idle) {
            TurnState::AwaitingApproval(pending) => {
                let result = self
                    .end_pending_script(
                        *pending,
                        ScriptExecutionStatus::Cancelled,
                        "Script cancelled before approval",
                    )
                    .await;
                answer(response, result.map(|()| true));
            }
            state @ TurnState::RunningScript(_) => {
                self.state = state;
                self.cancel_running_script(response);
            }
            TurnState::Preparing(mut preparation) => {
                preparation.cancel = Some(CancelRequest {
                    discarded,
                    response,
                });
                self.state = TurnState::Preparing(preparation);
            }
            TurnState::Streaming(job) => self.cancel_stream_job(job, discarded, response).await,
            TurnState::Idle => {
                let active = self.runtime.agent_manager.is_turn_active(&self.session_id);
                let cancelled = active || discarded;
                if cancelled {
                    self.finish_turn().await;
                    self.runtime.send_event(Event::HistoryUpdated {
                        session_id: self.session_id.clone(),
                    });
                    self.drain().await;
                }
                answer(response, Ok(cancelled));
            }
        }
    }

    async fn discard_queue(&self, discard: bool) -> Result<bool> {
        if !discard
            || !self
                .runtime
                .session_store
                .owns_turn(&self.session_id)
                .await?
        {
            return Ok(false);
        }
        self.runtime
            .session_store
            .clear_queued_messages(&self.session_id)
            .await
    }

    pub(super) async fn cancel_stream_job(
        &mut self,
        job: StreamJob,
        discarded: bool,
        response: Option<Responder<bool>>,
    ) {
        // The stream keeps running until the cancellation is durable, so a failed save can be retried.
        let output = kraai_script_protocol::render_tool_call_result(
            kraai_script_protocol::ToolCallResultView {
                status: ScriptExecutionStatus::Cancelled,
                exit_code: None,
                elapsed_millis: None,
                stdout: &[],
                stderr: &[],
                diagnostic: Some("Script was cancelled before execution"),
            },
        );
        let cancelled = match self
            .runtime
            .agent_manager
            .cancel_streaming_message(&job.message_id, &output)
            .await
        {
            Ok(cancelled) => cancelled,
            Err(error) => {
                if matches!(
                    self.runtime.session_store.owns_turn(&self.session_id).await,
                    Ok(false)
                ) {
                    job.cancel.cancel();
                    self.runtime
                        .stream_tasks
                        .wait_session(&self.session_id)
                        .await;
                    self.finish_turn().await;
                    self.runtime.send_session_error(&self.session_id, error);
                    answer(response, Ok(true));
                } else {
                    self.state = TurnState::Streaming(job);
                    answer(response, Err(error));
                }
                return;
            }
        };
        job.cancel.cancel();
        if cancelled.is_some() {
            self.runtime
                .stream_tasks
                .wait_session(&self.session_id)
                .await;
        }
        self.finish_turn().await;
        let Some(cancelled) = cancelled else {
            answer(response, Ok(discarded));
            return;
        };
        self.runtime.send_event(Event::StreamCancelled {
            session_id: cancelled.session_id.clone(),
            message_id: cancelled.message_id.to_string(),
        });
        self.runtime.send_event(Event::HistoryUpdated {
            session_id: cancelled.session_id,
        });
        self.drain().await;
        answer(response, Ok(true));
    }

    /// Returns true when the session was deleted and the actor should exit.
    pub(super) async fn delete(&mut self, response: Responder<()>) -> bool {
        match std::mem::replace(&mut self.state, TurnState::Idle) {
            state @ (TurnState::RunningScript(_) | TurnState::AwaitingApproval(_)) => {
                self.state = state;
                let _ = response.send(Err(RuntimeError::conflict(format!(
                    "Cannot delete session {} while a script is active",
                    self.session_id
                ))));
                return false;
            }
            state @ TurnState::Preparing(_) => {
                self.state = state;
                self.deferred
                    .push_back(super::actor::SessionMessage::Delete { response });
                return false;
            }
            TurnState::Streaming(job) => {
                job.cancel.cancel();
                let _ = job.task.await;
            }
            TurnState::Idle => {}
        }
        self.runtime
            .stream_tasks
            .wait_session(&self.session_id)
            .await;
        let result = self
            .runtime
            .agent_manager
            .delete_session(&self.session_id)
            .await;
        let deleted = result.is_ok();
        if deleted {
            self.runtime.release_turn(&self.session_id).await;
            self.runtime
                .observed_sessions
                .lock()
                .await
                .remove(&self.session_id);
            self.runtime.event_tx.remove_timer(&self.session_id);
        }
        let _ = response.send(result.map_err(RuntimeError::from_report));
        deleted
    }

    pub(super) async fn set_mcp_servers(&mut self, config: kraai_mcp::McpConfig) -> Result<()> {
        if !matches!(self.state, TurnState::Idle)
            || self
                .runtime
                .session_store
                .has_queued_messages(&self.session_id)
                .await?
        {
            return Err(eyre!(DomainError::conflict(
                "Cannot change MCP servers while the session has pending work"
            )));
        }
        let previous = self
            .runtime
            .agent_manager
            .replace_session_mcp_servers(&self.session_id, config)
            .await?;
        if let Some(previous) = previous {
            previous.shutdown().await;
        }
        Ok(())
    }
}

fn answer(response: Option<Responder<bool>>, result: Result<bool>) {
    if let Some(response) = response {
        let _ = response.send(result.map_err(RuntimeError::from_report));
    }
}
