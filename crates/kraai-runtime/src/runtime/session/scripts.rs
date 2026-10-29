use color_eyre::eyre::{Context, Result, eyre};
use kraai_execution_types::{
    PermissionResolution, SandboxCapabilities, SandboxCapability, ScriptExecutionId,
    ScriptExecutionStatus,
};
use kraai_persistence::{NewScriptExecution, ScriptExecutionCompletion};
use kraai_script_protocol::{InvalidScriptBlock, ProtocolError, ScriptBlock};
use kraai_types::{MessageId, ToolCallId};
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

use super::super::script_environment::{configured_runtime_roots, script_environment};
use super::super::script_execution::{
    CompletedScriptExecution, EffectiveScriptRequest, PendingScriptApproval,
};
use super::actor::{Responder, ScriptJob, SessionActor, SessionMessage, TurnState};
use crate::api::{Event, PendingScriptInfo};

fn host_failure(completed: &CompletedScriptExecution) -> color_eyre::Report {
    eyre!(
        completed
            .record
            .error
            .clone()
            .unwrap_or_else(|| String::from(
                "Nushell host is unavailable; rebuild both binaries with `just build`"
            ))
    )
}

#[derive(Clone, Copy)]
enum ScriptDecision {
    Allow,
    Deny,
    Prompt,
}

impl SessionActor {
    pub(super) async fn process_completed_stream_output(
        &mut self,
        source_message_id: MessageId,
        call_id: Option<ToolCallId>,
        script: Option<ScriptBlock>,
        invalid_script: Option<InvalidScriptBlock>,
        protocol_error: Option<ProtocolError>,
    ) {
        let call_id = match call_id {
            Some(call_id) => call_id,
            None if script.is_none() && protocol_error.is_none() => {
                self.finish_turn().await;
                self.runtime.send_event(Event::TurnCompleted {
                    session_id: self.session_id.clone(),
                });
                self.drain().await;
                return;
            }
            None => {
                self.fail_script_turn(&eyre!(
                    "completed script response did not contain a call id"
                ))
                .await;
                return;
            }
        };
        let result = match (protocol_error, script) {
            (Some(error), _) => {
                let invalid = invalid_script.unwrap_or(InvalidScriptBlock {
                    input: String::new(),
                    source: Vec::new(),
                    timeout: None,
                    requested_capabilities: SandboxCapabilities::default(),
                });
                self.finish_invalid_script(source_message_id, call_id, invalid, error)
                    .await
            }
            (None, Some(script)) => {
                self.prepare_or_execute_script(source_message_id, call_id, script)
                    .await
            }
            (None, None) => Ok(()),
        };
        if let Err(error) = result {
            self.fail_script_turn(&error).await;
        }
    }

    async fn prepare_or_execute_script(
        &mut self,
        source_message_id: MessageId,
        call_id: ToolCallId,
        script: ScriptBlock,
    ) -> Result<()> {
        let turn = self
            .runtime
            .agent_manager
            .script_turn_context(&self.session_id)?;
        let resolution = turn.profile.permissions.resolve(
            &script.requested_capabilities,
            &turn.profile.permission_rules,
            turn.profile.escalation_policy,
        );
        let (effective_capabilities, additions, decision) = match resolution {
            PermissionResolution::Denied { denied } => (
                turn.profile.permissions.capabilities().clone(),
                denied,
                ScriptDecision::Deny,
            ),
            PermissionResolution::Prompt { candidate } => (
                candidate.effective().clone(),
                candidate.additions().to_vec(),
                ScriptDecision::Prompt,
            ),
            PermissionResolution::Allowed(resolved) => (
                resolved.effective().clone(),
                resolved.additions().to_vec(),
                ScriptDecision::Allow,
            ),
        };
        let workspace = turn.workspace_dir.clone();
        let skill_roots =
            tokio::task::spawn_blocking(move || kraai_agent::discover_skill_read_roots(&workspace))
                .await?;
        let mut runtime_roots = self
            .runtime
            .config
            .script_runtime_roots
            .clone()
            .unwrap_or_else(configured_runtime_roots);
        runtime_roots.extend(skill_roots);
        let output_dir = self.runtime.config.script_output_dir(&self.session_id);
        kraai_io::fs::create_dir_all_async(&output_dir).await?;
        runtime_roots.push(output_dir);
        let request = EffectiveScriptRequest {
            id: ScriptExecutionId::new(Ulid::generate()),
            session_id: self.session_id.clone(),
            source_message_id,
            call_id,
            source: script.source,
            workspace_root: turn.workspace_dir,
            requested_capabilities: script.requested_capabilities,
            effective_capabilities,
            timeout: script.timeout,
            environment: script_environment(&turn.profile)?,
            runtime_roots,
            profile: turn.profile,
        };
        self.runtime.prepare_script_execution(&request).await?;
        self.runtime.send_event(Event::ScriptPrepared {
            session_id: self.session_id.clone(),
            execution_id: request.id.to_string(),
            call_id: request.call_id.to_string(),
            source: script.input,
        });

        match decision {
            ScriptDecision::Deny => {
                let denied = capability_names(&additions).join(", ");
                let completed = self
                    .finish_prepared_execution(
                        &request.id,
                        ScriptExecutionStatus::Denied,
                        Some(format!("Profile policy denied capabilities: {denied}")),
                    )
                    .await?;
                self.finalize_script_turn(completed).await
            }
            ScriptDecision::Prompt => {
                self.runtime
                    .execution_store
                    .mark_awaiting_approval(&request.id)
                    .await?;
                let pending = PendingScriptApproval { request, additions };
                let script = pending_script_info(&pending);
                self.state = TurnState::AwaitingApproval(Box::new(pending));
                self.runtime.send_event(Event::ScriptApprovalRequested {
                    session_id: self.session_id.clone(),
                    script,
                });
                Ok(())
            }
            ScriptDecision::Allow => {
                self.start_prepared_script(request);
                Ok(())
            }
        }
    }

    async fn finish_invalid_script(
        &mut self,
        source_message_id: MessageId,
        call_id: ToolCallId,
        invalid: InvalidScriptBlock,
        error: ProtocolError,
    ) -> Result<()> {
        let turn = self
            .runtime
            .agent_manager
            .script_turn_context(&self.session_id)?;
        let id = ScriptExecutionId::new(Ulid::generate());
        let effective_capabilities = turn.profile.permissions.capabilities().clone();
        self.runtime
            .execution_store
            .create(NewScriptExecution {
                id: id.clone(),
                session_id: self.session_id.clone(),
                source_message_id,
                call_id: call_id.clone(),
                profile: turn.profile,
                source: invalid.source,
                requested_capabilities: invalid.requested_capabilities,
                effective_capabilities,
                timeout: invalid.timeout,
            })
            .await?;
        self.runtime.send_event(Event::ScriptPrepared {
            session_id: self.session_id.clone(),
            execution_id: id.to_string(),
            call_id: call_id.to_string(),
            source: invalid.input,
        });
        let completed = self
            .finish_prepared_execution(
                &id,
                ScriptExecutionStatus::InvalidScript,
                Some(error.to_string()),
            )
            .await?;
        self.finalize_script_turn(completed).await
    }

    async fn finish_prepared_execution(
        &self,
        id: &ScriptExecutionId,
        status: ScriptExecutionStatus,
        error: Option<String>,
    ) -> Result<CompletedScriptExecution> {
        let store = &self.runtime.execution_store;
        let record = store
            .finish(
                id,
                ScriptExecutionCompletion {
                    status,
                    exit_code: None,
                    sandbox_denied: false,
                    error,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                },
            )
            .await?;
        let output = store.read_output(id).await?;
        Ok(CompletedScriptExecution { record, output })
    }

    pub(super) async fn finalize_script_turn(
        &mut self,
        completed: CompletedScriptExecution,
    ) -> Result<()> {
        let outcome = completed.record.outcome()?;
        let status = outcome.status;
        let execution_id = completed.record.id.to_string();
        let result = self.runtime.render_script_result(&completed).await?;
        self.runtime
            .agent_manager
            .add_script_result_to_history(
                &self.session_id,
                completed.record.result_message_id.clone(),
                completed.record.profile.id.clone(),
                completed.record.call_id.clone(),
                result.clone(),
                outcome,
            )
            .await
            .with_context(|| {
                format!("Failed to persist result for script execution {execution_id}")
            })?;
        self.runtime.send_event(Event::ScriptResultReady {
            session_id: self.session_id.clone(),
            execution_id,
            call_id: completed.record.call_id.to_string(),
            output: result,
            outcome,
        });
        let turn_active = self.runtime.agent_manager.is_turn_active(&self.session_id);
        self.state = TurnState::Idle;
        if status == ScriptExecutionStatus::HostUnavailable {
            self.fail_script_turn(&host_failure(&completed)).await;
        } else if status == ScriptExecutionStatus::Cancelled || !turn_active || self.stopping {
            self.finish_turn().await;
            self.drain().await;
        } else {
            self.continue_turn().await;
        }
        Ok(())
    }

    async fn fail_script_turn(&mut self, error: &color_eyre::Report) {
        self.finish_turn().await;
        self.drain().await;
        self.runtime.send_event(Event::ContinuationFailed {
            session_id: self.session_id.clone(),
            error: error.to_string(),
        });
    }

    fn start_prepared_script(&mut self, request: EffectiveScriptRequest) {
        self.runtime.event_tx.resume_timer(&self.session_id);
        let generation = self.generation();
        let cancel = CancellationToken::new();
        let runtime = self.runtime.clone();
        let execution_cancel = cancel.clone();
        let mailbox = self.mailbox();
        tokio::spawn(async move {
            let result = runtime
                .execute_prepared_script(request, execution_cancel)
                .await;
            let _ = mailbox.send(SessionMessage::ScriptEnded { generation, result });
        });
        self.state = TurnState::RunningScript(ScriptJob {
            generation,
            cancel,
            cancel_waiters: Vec::new(),
        });
    }

    pub(super) async fn script_ended(
        &mut self,
        generation: u64,
        result: Result<CompletedScriptExecution>,
    ) {
        let job = match std::mem::replace(&mut self.state, TurnState::Idle) {
            TurnState::RunningScript(job) if job.generation == generation => job,
            state => {
                self.state = state;
                return;
            }
        };
        let outcome = match result {
            Ok(completed) => self.finalize_script_turn(completed).await,
            Err(error) => Err(error),
        };
        if let Err(error) = outcome {
            self.fail_script_turn(&error).await;
        }
        for waiter in job.cancel_waiters {
            let _ = waiter.send(Ok(true));
        }
    }

    fn take_pending_script(
        &mut self,
        execution_id: &ScriptExecutionId,
    ) -> Result<PendingScriptApproval> {
        let pending_id = match &self.state {
            TurnState::AwaitingApproval(pending) => pending.request.id.clone(),
            _ => {
                return Err(eyre!(kraai_execution_types::DomainError::not_found(
                    format!("Session {} has no pending script approval", self.session_id)
                )));
            }
        };
        if &pending_id != execution_id {
            return Err(eyre!(kraai_execution_types::DomainError::conflict(
                format!(
                    "Pending execution for session {} is {pending_id}, not {execution_id}",
                    self.session_id
                )
            )));
        }
        match std::mem::replace(&mut self.state, TurnState::Idle) {
            TurnState::AwaitingApproval(pending) => Ok(*pending),
            state => {
                self.state = state;
                Err(eyre!("Pending script approval disappeared"))
            }
        }
    }

    pub(super) fn approve(&mut self, execution_id: &ScriptExecutionId) -> Result<()> {
        let pending = self.take_pending_script(execution_id)?;
        self.start_prepared_script(pending.request);
        Ok(())
    }

    pub(super) async fn deny(&mut self, execution_id: &ScriptExecutionId) -> Result<()> {
        let pending = self.take_pending_script(execution_id)?;
        self.runtime.event_tx.resume_timer(&self.session_id);
        self.end_pending_script(
            pending,
            ScriptExecutionStatus::Denied,
            "Capability escalation denied by user",
        )
        .await
    }

    pub(super) async fn end_pending_script(
        &mut self,
        pending: PendingScriptApproval,
        status: ScriptExecutionStatus,
        reason: &str,
    ) -> Result<()> {
        let result = async {
            let completed = self
                .finish_prepared_execution(&pending.request.id, status, Some(reason.to_string()))
                .await?;
            self.finalize_script_turn(completed).await
        }
        .await;
        if let Err(error) = &result {
            self.fail_script_turn(error).await;
        }
        result
    }

    pub(super) fn pending_script_info(&self) -> Option<PendingScriptInfo> {
        match &self.state {
            TurnState::AwaitingApproval(pending) => Some(pending_script_info(pending)),
            _ => None,
        }
    }

    /// Waits for a running script to stop after its cancellation was requested.
    pub(super) fn cancel_running_script(&mut self, response: Option<Responder<bool>>) {
        if let TurnState::RunningScript(job) = &mut self.state {
            job.cancel.cancel();
            self.runtime
                .agent_manager
                .clear_active_turn(&self.session_id);
            job.cancel_waiters.extend(response);
        }
    }
}

fn pending_script_info(pending: &PendingScriptApproval) -> PendingScriptInfo {
    PendingScriptInfo {
        execution_id: pending.request.id.to_string(),
        call_id: pending.request.call_id.to_string(),
        source: String::from_utf8_lossy(&pending.request.source).into_owned(),
        requested_capabilities: capability_names(
            &pending
                .request
                .requested_capabilities
                .iter()
                .collect::<Vec<_>>(),
        ),
        capability_additions: capability_names(&pending.additions),
        timeout_millis: u64::try_from(pending.request.timeout.as_millis()).unwrap_or(u64::MAX),
    }
}

fn capability_names(capabilities: &[SandboxCapability]) -> Vec<String> {
    capabilities
        .iter()
        .map(|capability| capability.as_str().to_string())
        .collect()
}
