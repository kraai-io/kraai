use color_eyre::eyre::{Context, Result, eyre};
use kraai_persistence::{NewScriptExecution, ScriptExecutionCompletion};
use kraai_script_protocol::{InvalidScriptBlock, ProtocolError, ScriptBlock};
use kraai_types::{
    EnvironmentPolicy, ModelId, PathPolicy, PermissionResolution, ProviderId, SandboxCapabilities,
    SandboxCapability, ScriptExecutionId, ScriptExecutionStatus, ScriptProfileSnapshot,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

use super::core::{ActiveScriptTask, QueuedMessage, RuntimeCore, emit_event};
use super::script_execution::{
    CompletedScriptExecution, EffectiveScriptRequest, PendingScriptApproval,
};
use super::streaming::StreamJobKind;
use crate::api::{Event, PendingScriptInfo};
use crate::handle::Command;

impl RuntimeCore {
    pub(crate) async fn recover_script_executions(&self) -> Result<()> {
        let records = self.execution_store.list_all().await?;
        let mut continuations = Vec::new();
        for record in records {
            let completed = if record.phase == kraai_types::ScriptExecutionPhase::Finished {
                CompletedScriptExecution {
                    output: self.execution_store.read_output(&record.id).await?,
                    record,
                }
            } else {
                let output = self.execution_store.read_output(&record.id).await?;
                let record = self
                    .execution_store
                    .finish(
                        &record.id,
                        ScriptExecutionCompletion {
                            status: ScriptExecutionStatus::RuntimeError,
                            exit_code: None,
                            sandbox_denied: record.sandbox_denied,
                            error: Some(format!(
                                "Kraai stopped while script execution was in phase {:?}",
                                record.phase
                            )),
                            stdout: output.stdout.clone(),
                            stderr: output.stderr.clone(),
                        },
                    )
                    .await?;
                CompletedScriptExecution { record, output }
            };

            let result = completed.render_result()?;
            let result_message_id = completed.record.result_message_id.clone();
            let session_id = completed.record.session_id.clone();
            self.agent_manager
                .lock()
                .await
                .add_script_result_to_history(
                    &session_id,
                    result_message_id.clone(),
                    completed.record.profile.id.clone(),
                    result,
                )
                .await?;

            let tip = self.agent_manager.lock().await.get_tip(&session_id).await?;
            if tip.as_ref() == Some(&result_message_id) {
                self.agent_manager
                    .lock()
                    .await
                    .prepare_script_recovery(&session_id, &completed.record.source_message_id)
                    .await?;
                continuations.push(session_id);
            }
        }
        continuations.sort();
        continuations.dedup();
        for session_id in continuations {
            self.spawn_continuation(session_id);
        }
        Ok(())
    }

    pub(crate) async fn handle_send_message(
        &self,
        session_id: String,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
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
                },
            )
            .await;
            self.schedule_queue_drain(&session_id).await;
            return;
        }

        let stream_request = {
            let mut agent = self.agent_manager.lock().await;
            let result = agent
                .prepare_start_stream(&session_id, message, model_id, provider_id)
                .await;
            let providers = agent.cloned_provider_manager();
            drop(agent);
            match result {
                Ok(result) => Some((providers, result)),
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
                let result = agent
                    .prepare_start_stream(
                        &session_id,
                        next_message.message,
                        next_message.model_id,
                        next_message.provider_id,
                    )
                    .await;
                let providers = agent.cloned_provider_manager();
                drop(agent);
                match result {
                    Ok(result) => Some((providers, result)),
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
        _content: String,
        script: Option<ScriptBlock>,
        invalid_script: Option<InvalidScriptBlock>,
        protocol_error: Option<ProtocolError>,
    ) {
        if let Some(error) = protocol_error {
            let invalid = invalid_script.unwrap_or(InvalidScriptBlock {
                source: Vec::new(),
                timeout: None,
                requested_capabilities: SandboxCapabilities::default(),
            });
            if let Err(failure) = self
                .finish_invalid_script(&completed_session, source_message_id, invalid, error)
                .await
            {
                self.fail_script_turn(&completed_session, failure).await;
            }
            return;
        }

        let Some(script) = script else {
            let mut agent = self.agent_manager.lock().await;
            agent.clear_active_turn(&completed_session);
            drop(agent);
            self.schedule_queue_drain(&completed_session).await;
            return;
        };

        if let Err(error) = self
            .prepare_or_execute_script(completed_session.clone(), source_message_id, script)
            .await
        {
            self.fail_script_turn(&completed_session, error).await;
        }
    }
}

impl RuntimeCore {
    async fn prepare_or_execute_script(
        &self,
        session_id: String,
        source_message_id: kraai_types::MessageId,
        script: ScriptBlock,
    ) -> Result<()> {
        let turn = self
            .agent_manager
            .lock()
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
        let request = EffectiveScriptRequest {
            id: ScriptExecutionId::new(Ulid::new()),
            session_id: session_id.clone(),
            source_message_id,
            profile: turn.profile.clone(),
            source: script.source,
            workspace_root: turn.workspace_dir,
            requested_capabilities: script.requested_capabilities,
            effective_capabilities,
            timeout: script.timeout,
            environment: script_environment(&turn.profile)?,
            runtime_roots: configured_runtime_roots(),
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
        invalid: InvalidScriptBlock,
        error: ProtocolError,
    ) -> Result<()> {
        let turn = self
            .agent_manager
            .lock()
            .await
            .script_turn_context(session_id)?;
        let id = ScriptExecutionId::new(Ulid::new());
        self.execution_store
            .create(NewScriptExecution {
                id: id.clone(),
                session_id: session_id.to_string(),
                source_message_id,
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

    async fn finalize_script_turn(
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
            .lock()
            .await
            .add_script_result_to_history(
                session_id,
                completed.record.result_message_id.clone(),
                completed.record.profile.id.clone(),
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
        emit_event(
            &self.event_tx,
            Event::HistoryUpdated {
                session_id: session_id.to_string(),
            },
        );
        if status == ScriptExecutionStatus::Cancelled {
            let mut agent = self.agent_manager.lock().await;
            agent.clear_active_turn(session_id);
            drop(agent);
            self.schedule_queue_drain(session_id).await;
        } else {
            self.spawn_continuation(session_id.to_string());
        }
        Ok(())
    }

    async fn fail_script_turn(&self, session_id: &str, error: color_eyre::Report) {
        {
            let mut agent = self.agent_manager.lock().await;
            agent.clear_active_turn(session_id);
        }
        self.schedule_queue_drain(session_id).await;
        emit_event(&self.event_tx, Event::Error(error.to_string()));
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
            return Err(eyre!("Session {session_id} already has an active script"));
        }

        let runtime = self.clone();
        let task_session_id = session_id.clone();
        let cancellation = CancellationToken::new();
        let execution_cancellation = cancellation.clone();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            match runtime
                .execute_prepared_script(request, execution_cancellation)
                .await
            {
                Ok(completed) => {
                    runtime
                        .active_script_tasks
                        .lock()
                        .await
                        .remove(&task_session_id);
                    if let Err(error) = runtime
                        .finalize_script_turn(&task_session_id, completed)
                        .await
                    {
                        runtime.fail_script_turn(&task_session_id, error).await;
                    }
                }
                Err(error) => {
                    runtime
                        .active_script_tasks
                        .lock()
                        .await
                        .remove(&task_session_id);
                    runtime.fail_script_turn(&task_session_id, error).await;
                }
            }
        });
        let mut active_tasks = self.active_script_tasks.lock().await;
        match active_tasks.entry(session_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(ActiveScriptTask {
                    cancellation,
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
        let Some(task) = self.active_script_tasks.lock().await.remove(session_id) else {
            return false;
        };
        task.cancellation.cancel();
        let _ = task.join_handle.await;
        true
    }

    pub(crate) async fn deny_pending_script(
        &self,
        session_id: String,
        execution_id: ScriptExecutionId,
    ) -> Result<()> {
        let pending = self.take_pending_script(&session_id, &execution_id).await?;
        let completed = self
            .finish_prepared_execution(
                &pending.request.id,
                ScriptExecutionStatus::Denied,
                Some(String::from("Capability escalation denied by user")),
            )
            .await?;
        self.finalize_script_turn(&session_id, completed).await
    }

    async fn take_pending_script(
        &self,
        session_id: &str,
        execution_id: &ScriptExecutionId,
    ) -> Result<PendingScriptApproval> {
        let mut pending = self.pending_script_approvals.lock().await;
        let Some(existing) = pending.get(session_id) else {
            return Err(eyre!("Session {session_id} has no pending script approval"));
        };
        if &existing.request.id != execution_id {
            return Err(eyre!(
                "Pending execution for session {session_id} is {}, not {execution_id}",
                existing.request.id
            ));
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

fn configured_runtime_roots() -> Vec<PathBuf> {
    std::env::var_os("KRAAI_SCRIPT_RUNTIME_ROOTS")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default()
}

fn script_environment(profile: &ScriptProfileSnapshot) -> Result<BTreeMap<String, String>> {
    const MINIMAL: &[&str] = &["LANG", "LANGUAGE", "LC_ALL", "LC_CTYPE", "TERM", "TZ"];
    const ALLOWED: &[&str] = &[
        "COLORTERM",
        "EDITOR",
        "HOME",
        "LOGNAME",
        "PAGER",
        "SHELL",
        "USER",
        "VISUAL",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
    ];
    let mut environment = BTreeMap::new();
    match profile.environment {
        EnvironmentPolicy::Minimal => copy_environment(MINIMAL, &mut environment),
        EnvironmentPolicy::AllowList => {
            copy_environment(MINIMAL, &mut environment);
            copy_environment(ALLOWED, &mut environment);
        }
        EnvironmentPolicy::Inherit => {
            for (name, value) in std::env::vars_os() {
                let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else {
                    continue;
                };
                environment.insert(name.to_string(), value.to_string());
            }
        }
    }
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = match profile.path {
        PathPolicy::Inherit => inherited_path,
        PathPolicy::Packaged => {
            let mut entries = Vec::new();
            if let Some(directory) = std::env::current_exe()
                .ok()
                .and_then(|executable| executable.parent().map(PathBuf::from))
            {
                entries.push(directory);
            }
            entries.extend(std::env::split_paths(&inherited_path));
            std::env::join_paths(entries).context("Failed to construct packaged script PATH")?
        }
    };
    let path = path
        .into_string()
        .map_err(|_error| eyre!("Script PATH contains non-UTF-8 data"))?;
    environment.insert(String::from("PATH"), path);
    Ok(environment)
}

fn copy_environment(names: &[&str], target: &mut BTreeMap<String, String>) {
    for name in names {
        if let Ok(value) = std::env::var(name) {
            target.insert((*name).to_string(), value);
        }
    }
}
