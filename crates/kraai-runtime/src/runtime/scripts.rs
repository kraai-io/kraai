use color_eyre::eyre::{Context, Result, eyre};
use kraai_persistence::{NewScriptExecution, ScriptExecutionCompletion};
use kraai_script_protocol::{InvalidScriptBlock, ProtocolError, ScriptBlock};
use kraai_types::{
    PermissionResolution, SandboxCapabilities, SandboxCapability, ScriptExecutionId,
    ScriptExecutionStatus,
};
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

use super::core::{ActiveScriptTask, RuntimeCore, emit_event};
use super::script_environment::{configured_runtime_roots, script_environment};
use super::script_execution::{
    CompletedScriptExecution, EffectiveScriptRequest, PendingScriptApproval,
};
use crate::api::{Event, PendingScriptInfo};

pub(super) fn host_failure(completed: &CompletedScriptExecution) -> color_eyre::Report {
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

impl RuntimeCore {
    pub(crate) async fn has_active_script_tasks(&self, session_id: &str) -> bool {
        let mut active_tasks = self.active_script_tasks.lock().await;
        let Some(task) = active_tasks.get(session_id) else {
            return false;
        };
        let has_active = !task.join_handle.is_finished();
        if !has_active {
            active_tasks.remove(session_id);
        }
        has_active
    }

    pub(crate) async fn process_completed_stream_output(
        &self,
        completed_session: String,
        source_message_id: kraai_types::MessageId,
        call_id: Option<kraai_types::ToolCallId>,
        script: Option<ScriptBlock>,
        invalid_script: Option<InvalidScriptBlock>,
        protocol_error: Option<ProtocolError>,
    ) {
        let call_id = match call_id {
            Some(call_id) => call_id,
            None if script.is_none() && protocol_error.is_none() => {
                let mut agent = self.agent_manager.write().await;
                agent.clear_active_turn(&completed_session);
                drop(agent);
                self.event_tx.finish_timer(&completed_session);
                self.schedule_queue_drain(&completed_session);
                emit_event(
                    &self.event_tx,
                    Event::HistoryUpdated {
                        session_id: completed_session,
                    },
                );
                return;
            }
            None => {
                self.fail_script_turn(
                    &completed_session,
                    &eyre!("completed script response did not contain a call id"),
                )
                .await;
                return;
            }
        };
        if let Some(error) = protocol_error {
            let invalid = invalid_script.unwrap_or(InvalidScriptBlock {
                input: String::new(),
                source: Vec::new(),
                timeout: None,
                requested_capabilities: SandboxCapabilities::default(),
            });
            if let Err(failure) = self
                .finish_invalid_script(
                    &completed_session,
                    source_message_id,
                    call_id,
                    invalid,
                    error,
                )
                .await
            {
                self.fail_script_turn(&completed_session, &failure).await;
            }
            return;
        }

        let Some(script) = script else { return };

        if let Err(error) = self
            .prepare_or_execute_script(
                completed_session.clone(),
                source_message_id,
                call_id,
                script,
            )
            .await
        {
            self.fail_script_turn(&completed_session, &error).await;
        }
    }
}

impl RuntimeCore {
    async fn prepare_or_execute_script(
        &self,
        session_id: String,
        source_message_id: kraai_types::MessageId,
        call_id: kraai_types::ToolCallId,
        script: ScriptBlock,
    ) -> Result<()> {
        let turn = self
            .agent_manager
            .read()
            .await
            .script_turn_context(&session_id)?;
        let resolution = turn
            .profile
            .permissions
            .resolve(
                &script.requested_capabilities,
                &turn.profile.permission_rules,
                turn.profile.escalation_policy,
            )
            .map_err(|error| eyre!(error))?;

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
            .script_runtime_roots
            .clone()
            .unwrap_or_else(configured_runtime_roots);
        runtime_roots.extend(skill_roots);
        let request = EffectiveScriptRequest {
            id: ScriptExecutionId::new(Ulid::generate()),
            session_id: session_id.clone(),
            source_message_id,
            call_id,
            profile: turn.profile.clone(),
            source: script.source,
            workspace_root: turn.workspace_dir,
            requested_capabilities: script.requested_capabilities,
            effective_capabilities,
            timeout: script.timeout,
            environment: script_environment(&turn.profile)?,
            runtime_roots,
            active_commands: turn.profile.commands.clone(),
        };
        self.prepare_script_execution(&request).await?;

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
                self.finalize_script_turn(&session_id, completed).await
            }
            ScriptDecision::Prompt => {
                self.execution_store
                    .mark_awaiting_approval(&request.id)
                    .await?;
                let pending = PendingScriptApproval { request, additions };
                let info = pending_script_info(&pending);
                let previous = self
                    .pending_script_approvals
                    .lock()
                    .await
                    .insert(session_id.clone(), pending);
                if previous.is_some() {
                    return Err(eyre!(
                        "Session {session_id} already has a pending script approval"
                    ));
                }
                emit_event(
                    &self.event_tx,
                    Event::ScriptApprovalRequested {
                        session_id,
                        script: info,
                    },
                );
                Ok(())
            }
            ScriptDecision::Allow => self.start_prepared_script(session_id, request).await,
        }
    }

    async fn finish_invalid_script(
        &self,
        session_id: &str,
        source_message_id: kraai_types::MessageId,
        call_id: kraai_types::ToolCallId,
        invalid: InvalidScriptBlock,
        error: ProtocolError,
    ) -> Result<()> {
        let turn = self
            .agent_manager
            .read()
            .await
            .script_turn_context(session_id)?;
        let id = ScriptExecutionId::new(Ulid::generate());
        self.execution_store
            .create(NewScriptExecution {
                id: id.clone(),
                session_id: session_id.to_string(),
                source_message_id,
                call_id,
                profile: turn.profile.clone(),
                source: invalid.source,
                requested_capabilities: invalid.requested_capabilities,
                effective_capabilities: turn.profile.permissions.capabilities().clone(),
                timeout: invalid.timeout,
            })
            .await?;
        let completed = self
            .finish_prepared_execution(
                &id,
                ScriptExecutionStatus::InvalidScript,
                Some(error.to_string()),
            )
            .await?;
        self.finalize_script_turn(session_id, completed).await
    }

    async fn finish_prepared_execution(
        &self,
        id: &ScriptExecutionId,
        status: ScriptExecutionStatus,
        error: Option<String>,
    ) -> Result<CompletedScriptExecution> {
        let record = self
            .execution_store
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
        let output = self.execution_store.read_output(id).await?;
        Ok(CompletedScriptExecution { record, output })
    }

    pub(super) async fn finalize_script_turn(
        &self,
        session_id: &str,
        completed: CompletedScriptExecution,
    ) -> Result<()> {
        let status = completed
            .record
            .status
            .ok_or_else(|| eyre!("Completed execution has no terminal status"))?;
        let execution_id = completed.record.id.to_string();
        let result = completed.render_result()?;
        self.agent_manager
            .write()
            .await
            .add_script_result_to_history(
                session_id,
                completed.record.result_message_id.clone(),
                completed.record.profile.id.clone(),
                completed.record.call_id.clone(),
                result,
            )
            .await
            .with_context(|| {
                format!("Failed to persist result for script execution {execution_id}")
            })?;
        emit_event(
            &self.event_tx,
            Event::ScriptResultReady {
                session_id: session_id.to_string(),
                execution_id,
                status: status.as_str().to_string(),
            },
        );
        if status == ScriptExecutionStatus::HostUnavailable {
            self.fail_script_turn(session_id, &host_failure(&completed))
                .await;
        } else if status == ScriptExecutionStatus::Cancelled {
            let mut agent = self.agent_manager.write().await;
            agent.clear_active_turn(session_id);
            drop(agent);
            self.event_tx.finish_timer(session_id);
            self.schedule_queue_drain(session_id);
        } else {
            self.spawn_continuation(session_id.to_string());
        }
        emit_event(
            &self.event_tx,
            Event::HistoryUpdated {
                session_id: session_id.to_string(),
            },
        );
        Ok(())
    }

    pub(super) async fn fail_script_turn(&self, session_id: &str, error: &color_eyre::Report) {
        {
            let mut agent = self.agent_manager.write().await;
            agent.clear_active_turn(session_id);
            drop(agent);
            self.event_tx.finish_timer(session_id);
        }
        self.schedule_queue_drain(session_id);
        emit_event(
            &self.event_tx,
            Event::ContinuationFailed {
                session_id: session_id.to_string(),
                error: error.to_string(),
            },
        );
    }

    pub(crate) async fn get_pending_script(&self, session_id: &str) -> Option<PendingScriptInfo> {
        self.pending_script_approvals
            .lock()
            .await
            .get(session_id)
            .map(pending_script_info)
    }

    pub(crate) async fn approve_pending_script(
        &self,
        session_id: String,
        execution_id: ScriptExecutionId,
    ) -> Result<()> {
        let pending = self.take_pending_script(&session_id, &execution_id).await?;
        self.start_prepared_script(session_id, pending.request)
            .await
    }

    async fn start_prepared_script(
        &self,
        session_id: String,
        request: EffectiveScriptRequest,
    ) -> Result<()> {
        if self.has_active_script_tasks(&session_id).await {
            return Err(eyre!(kraai_types::DomainError::conflict(format!(
                "Session {session_id} already has an active script"
            ))));
        }

        self.event_tx.resume_timer(&session_id);
        let runtime = self.clone();
        let task_session_id = session_id.clone();
        let cancellation = CancellationToken::new();
        let execution_cancellation = cancellation.clone();
        let completion = CancellationToken::new();
        let completion_guard = completion.clone().drop_guard();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _completion_guard = completion_guard;
            if start_rx.await.is_err() {
                return;
            }
            match runtime
                .execute_prepared_script(request, execution_cancellation)
                .await
            {
                Ok(completed) => {
                    let _state_guard = runtime.session_state_barrier.read().await;
                    runtime
                        .active_script_tasks
                        .lock()
                        .await
                        .remove(&task_session_id);
                    if let Err(error) = runtime
                        .finalize_script_turn(&task_session_id, completed)
                        .await
                    {
                        runtime.fail_script_turn(&task_session_id, &error).await;
                    }
                }
                Err(error) => {
                    let _state_guard = runtime.session_state_barrier.read().await;
                    runtime
                        .active_script_tasks
                        .lock()
                        .await
                        .remove(&task_session_id);
                    runtime.fail_script_turn(&task_session_id, &error).await;
                }
            }
        });
        let mut active_tasks = self.active_script_tasks.lock().await;
        match active_tasks.entry(session_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(ActiveScriptTask {
                    cancellation,
                    completion,
                    join_handle: task,
                });
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                drop(active_tasks);
                cancellation.cancel();
                drop(start_tx);
                let _ = task.await;
                return Err(eyre!(
                    "Active script state changed while starting execution"
                ));
            }
        }
        let _ = start_tx.send(());
        Ok(())
    }

    pub(crate) async fn cancel_active_script(&self, session_id: &str) -> bool {
        let completion = {
            let _state_guard = self.session_state_barrier.read().await;
            let tasks = self.active_script_tasks.lock().await;
            let Some(task) = tasks.get(session_id) else {
                return false;
            };
            task.cancellation.cancel();
            let completion = task.completion.clone();
            drop(tasks);
            completion
        };
        // Finalization owns removal and publishes terminal events under its own guard.
        // Await it without retaining a reader that could deadlock a queued snapshot writer.
        completion.cancelled().await;
        true
    }

    pub(crate) async fn deny_pending_script(
        &self,
        session_id: String,
        execution_id: ScriptExecutionId,
    ) -> Result<()> {
        let pending = self.take_pending_script(&session_id, &execution_id).await?;
        self.event_tx.resume_timer(&session_id);
        let result = async {
            let completed = self
                .finish_prepared_execution(
                    &pending.request.id,
                    ScriptExecutionStatus::Denied,
                    Some(String::from("Capability escalation denied by user")),
                )
                .await?;
            self.finalize_script_turn(&session_id, completed).await
        }
        .await;
        if let Err(error) = &result {
            self.fail_script_turn(&session_id, error).await;
        }
        result
    }

    async fn take_pending_script(
        &self,
        session_id: &str,
        execution_id: &ScriptExecutionId,
    ) -> Result<PendingScriptApproval> {
        let mut pending = self.pending_script_approvals.lock().await;
        let Some(existing) = pending.get(session_id) else {
            return Err(eyre!(kraai_types::DomainError::not_found(format!(
                "Session {session_id} has no pending script approval"
            ))));
        };
        if &existing.request.id != execution_id {
            return Err(eyre!(kraai_types::DomainError::conflict(format!(
                "Pending execution for session {session_id} is {}, not {execution_id}",
                existing.request.id
            ))));
        }
        pending
            .remove(session_id)
            .ok_or_else(|| eyre!("Pending script approval disappeared for session {session_id}"))
    }
}

#[derive(Clone, Copy)]
enum ScriptDecision {
    Allow,
    Deny,
    Prompt,
}

fn pending_script_info(pending: &PendingScriptApproval) -> PendingScriptInfo {
    PendingScriptInfo {
        execution_id: pending.request.id.to_string(),
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
