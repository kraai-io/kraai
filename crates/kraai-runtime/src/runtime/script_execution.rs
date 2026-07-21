use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::{Context, Result, eyre};
use kraai_nushell_runtime::{RuntimeError, ScriptExecutionPlan, StateEffectHandler};
use kraai_persistence::{
    ContextStateMutation, ContextStateStore, NewScriptExecution, PersistedScriptOutput,
    PinnedFileScope, ScriptExecutionCompletion, ScriptExecutionRecord,
};
use kraai_sandbox::{OutputEvent, OutputStream, SandboxError, Termination};
use kraai_script_protocol::{ToolCallResultView, render_tool_call_result};
use kraai_types::{
    MessageId, SandboxCapabilities, SandboxCapability, ScriptExecutionId, ScriptExecutionStatus,
    ScriptOutputStream, ScriptProfileSnapshot, StateEffectRequest,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

use super::core::RuntimeCore;

#[derive(Clone)]
pub(crate) struct EffectiveScriptRequest {
    pub(crate) id: ScriptExecutionId,
    pub(crate) session_id: String,
    pub(crate) source_message_id: MessageId,
    pub(crate) profile: ScriptProfileSnapshot,
    pub(crate) source: Vec<u8>,
    pub(crate) workspace_root: PathBuf,
    pub(crate) requested_capabilities: SandboxCapabilities,
    pub(crate) effective_capabilities: SandboxCapabilities,
    pub(crate) timeout: Duration,
    pub(crate) environment: BTreeMap<String, String>,
    pub(crate) runtime_roots: Vec<PathBuf>,
    pub(crate) active_commands: Vec<String>,
}

pub(crate) struct CompletedScriptExecution {
    pub(crate) record: ScriptExecutionRecord,
    pub(crate) output: PersistedScriptOutput,
}

#[derive(Clone)]
pub(crate) struct PendingScriptApproval {
    pub(crate) request: EffectiveScriptRequest,
    pub(crate) additions: Vec<SandboxCapability>,
}

impl CompletedScriptExecution {
    pub(crate) fn render_result(&self) -> Result<String> {
        let status = self.record.status.ok_or_else(|| {
            eyre!(
                "Execution {} finished without a terminal status",
                self.record.id
            )
        })?;
        Ok(render_tool_call_result(ToolCallResultView {
            status,
            exit_code: self.record.exit_code,
            stdout: &self.output.stdout,
            stderr: &self.output.stderr,
            diagnostic: self.record.error.as_deref(),
        }))
    }
}

impl RuntimeCore {
    pub(crate) async fn prepare_script_execution(
        &self,
        request: &EffectiveScriptRequest,
    ) -> Result<ScriptExecutionRecord> {
        self.execution_store
            .create(NewScriptExecution {
                id: request.id.clone(),
                session_id: request.session_id.clone(),
                source_message_id: request.source_message_id.clone(),
                profile: request.profile.clone(),
                source: request.source.clone(),
                requested_capabilities: request.requested_capabilities.clone(),
                effective_capabilities: request.effective_capabilities.clone(),
                timeout: Some(request.timeout),
            })
            .await
            .with_context(|| format!("Failed to create execution record {}", request.id))
    }

    pub(crate) async fn execute_prepared_script(
        &self,
        request: EffectiveScriptRequest,
        cancellation: CancellationToken,
    ) -> Result<CompletedScriptExecution> {
        let execution_id = request.id.clone();
        let host_executable = match resolve_nushell_host() {
            Ok(host) => host,
            Err(error) => {
                let record = self
                    .execution_store
                    .finish(
                        &execution_id,
                        ScriptExecutionCompletion {
                            status: ScriptExecutionStatus::FailedToStart,
                            exit_code: None,
                            sandbox_denied: false,
                            error: Some(error.to_string()),
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                        },
                    )
                    .await?;
                let output = self.execution_store.read_output(&execution_id).await?;
                return Ok(CompletedScriptExecution { record, output });
            }
        };
        self.execution_store
            .mark_running(&execution_id)
            .await
            .with_context(|| format!("Failed to mark execution {execution_id} running"))?;
        let (output_tx, output_rx) = tokio::sync::mpsc::unbounded_channel();
        let output_store = self.execution_store.clone();
        let output_execution_id = execution_id.clone();
        let output_task = tokio::spawn(persist_output_events(
            output_store,
            output_execution_id,
            output_rx,
        ));

        let mut plan = ScriptExecutionPlan::new(
            execution_id.clone(),
            host_executable,
            request.source,
            request.workspace_root.clone(),
            request.effective_capabilities.clone(),
            request.timeout,
        );
        plan.environment = request.environment;
        plan.runtime_roots = request.runtime_roots;
        if let Some(host_directory) = plan.host_executable.parent()
            && !plan.runtime_roots.iter().any(|root| root == host_directory)
        {
            plan.runtime_roots.push(host_directory.to_path_buf());
        }
        plan.active_commands = request.active_commands;
        plan.nushell_startup = request.profile.nushell_startup;
        plan.output_events = Some(output_tx);
        plan.state_effect_handler = Arc::new(DurableStateEffects {
            execution_id: execution_id.clone(),
            session_id: request.session_id,
            workspace_root: request.workspace_root,
            effective_capabilities: request.effective_capabilities,
            store: self.context_state_store.clone(),
        });

        let execution = kraai_nushell_runtime::execute(plan, cancellation).await;
        let output_persistence_error = output_task
            .await
            .map_err(|error| eyre!("output persistence task failed: {error}"))?
            .err();

        let completion = match execution {
            Ok(result) => completion_from_output(result.output, output_persistence_error),
            Err(error) => {
                let output = self.execution_store.read_output(&execution_id).await?;
                completion_from_runtime_error(error, output, output_persistence_error)
            }
        };
        let record = self
            .execution_store
            .finish(&execution_id, completion)
            .await
            .with_context(|| format!("Failed to finish execution {execution_id}"))?;
        let output = self.execution_store.read_output(&execution_id).await?;
        Ok(CompletedScriptExecution { record, output })
    }
}

struct DurableStateEffects {
    execution_id: ScriptExecutionId,
    session_id: String,
    workspace_root: PathBuf,
    effective_capabilities: SandboxCapabilities,
    store: Arc<dyn ContextStateStore>,
}

impl StateEffectHandler for DurableStateEffects {
    fn apply<'a>(
        &'a self,
        request: &'a StateEffectRequest,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            let mutations = authorize_context_mutations(
                request,
                &self.workspace_root,
                &self.effective_capabilities,
            )?;
            self.store
                .append_command(
                    &self.session_id,
                    &self.execution_id,
                    request.sequence,
                    &request.invocation_id,
                    &request.command_id,
                    mutations,
                )
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }
}

fn authorize_context_mutations(
    request: &StateEffectRequest,
    workspace_root: &Path,
    effective_capabilities: &SandboxCapabilities,
) -> std::result::Result<Vec<ContextStateMutation>, String> {
    request
        .deltas
        .iter()
        .map(|delta| {
            if delta.namespace != "opened_files" {
                return Err(format!(
                    "command '{}' requested unsupported context namespace '{}'",
                    request.command_id, delta.namespace
                ));
            }
            let path = delta
                .payload
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
                .ok_or_else(|| String::from("opened-files mutation requires a string path"))?;
            if !path.is_absolute() {
                return Err(format!(
                    "opened-files mutation path must be absolute: {}",
                    path.display()
                ));
            }
            match (request.command_id.as_str(), delta.operation.as_str()) {
                ("kraai-open-files", "open") => {
                    let scope = if path.starts_with(workspace_root) {
                        PinnedFileScope::Workspace {
                            root: workspace_root.to_path_buf(),
                        }
                    } else if effective_capabilities.contains(SandboxCapability::HostRead) {
                        PinnedFileScope::Host
                    } else {
                        return Err(format!(
                            "cannot pin host path without host-read: {}",
                            path.display()
                        ));
                    };
                    Ok(ContextStateMutation::PinFile { path, scope })
                }
                ("kraai-close-files", "close") => {
                    Ok(ContextStateMutation::UnpinFile { path, reason: None })
                }
                _ => Err(format!(
                    "command '{}' cannot apply opened-files operation '{}'",
                    request.command_id, delta.operation
                )),
            }
        })
        .collect()
}

async fn persist_output_events(
    store: Arc<dyn kraai_persistence::ScriptExecutionStore>,
    execution_id: ScriptExecutionId,
    mut events: UnboundedReceiver<OutputEvent>,
) -> std::result::Result<(), String> {
    let mut first_error = None;
    while let Some(event) = events.recv().await {
        let stream = match event.stream {
            OutputStream::Stdout => ScriptOutputStream::Stdout,
            OutputStream::Stderr => ScriptOutputStream::Stderr,
        };
        if let Err(error) = store
            .append_output(&execution_id, stream, &event.bytes)
            .await
            && first_error.is_none()
        {
            first_error = Some(error.to_string());
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn completion_from_output(
    output: kraai_sandbox::ExecutionOutput,
    output_persistence_error: Option<String>,
) -> ScriptExecutionCompletion {
    let status = if output_persistence_error.is_some() {
        ScriptExecutionStatus::RuntimeError
    } else {
        match output.termination {
            Termination::Exited { .. } => ScriptExecutionStatus::Completed,
            Termination::TimedOut => ScriptExecutionStatus::TimedOut,
            Termination::Cancelled => ScriptExecutionStatus::Cancelled,
        }
    };
    let exit_code = match output.termination {
        Termination::Exited { code } => code,
        Termination::TimedOut | Termination::Cancelled => None,
    };
    ScriptExecutionCompletion {
        status,
        exit_code,
        sandbox_denied: output.sandbox_denied,
        error: output_persistence_error
            .map(|error| format!("Failed to persist live script output: {error}")),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn completion_from_runtime_error(
    error: RuntimeError,
    output: PersistedScriptOutput,
    output_persistence_error: Option<String>,
) -> ScriptExecutionCompletion {
    let status = match &error {
        RuntimeError::Sandbox(SandboxError::SandboxUnavailable(_)) => {
            ScriptExecutionStatus::SandboxUnavailable
        }
        RuntimeError::Transport(_)
        | RuntimeError::Sandbox(
            SandboxError::ExecutableMustBeAbsolute
            | SandboxError::ExecutableNotVisible(_)
            | SandboxError::InvalidTimeout
            | SandboxError::WorkspaceReadRequired
            | SandboxError::MissingWorkspace(_)
            | SandboxError::InvalidRuntimeRoot(_)
            | SandboxError::PrivateTemp(_)
            | SandboxError::Spawn { .. },
        ) => ScriptExecutionStatus::FailedToStart,
        RuntimeError::RequestChannel(_)
        | RuntimeError::ChannelTask(_)
        | RuntimeError::EffectChannel(_)
        | RuntimeError::Sandbox(SandboxError::Wait(_)) => ScriptExecutionStatus::RuntimeError,
    };
    let mut diagnostic = error.to_string();
    if let Some(output_error) = output_persistence_error {
        diagnostic.push_str("; failed to persist live output: ");
        diagnostic.push_str(&output_error);
    }
    ScriptExecutionCompletion {
        status,
        exit_code: None,
        sandbox_denied: false,
        error: Some(diagnostic),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn resolve_nushell_host() -> Result<PathBuf> {
    let current_executable = std::env::current_exe()
        .context("Failed to locate the running Kraai executable")?
        .canonicalize()
        .context("Failed to canonicalize the running Kraai executable")?;
    let directory = current_executable.parent().ok_or_else(|| {
        eyre!(
            "Kraai executable has no parent directory: {}",
            current_executable.display()
        )
    })?;
    let host = directory.join("kraai-nushell-host");
    canonical_executable(&host).with_context(|| {
        format!(
            "Unable to locate the packaged Nushell host beside Kraai at {}",
            host.display()
        )
    })
}

fn canonical_executable(path: &Path) -> Result<PathBuf> {
    let canonical = path.canonicalize()?;
    if !canonical.is_file() {
        return Err(eyre!("Nushell host is not a file: {}", canonical.display()));
    }
    Ok(canonical)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "context mutation authorization tests use direct fixture assertions"
)]
mod tests {
    use super::*;
    use kraai_types::{CommandInvocationId, ContextStateDelta};
    use ulid::Ulid;

    fn open_request(path: &str) -> StateEffectRequest {
        StateEffectRequest {
            sequence: 1,
            invocation_id: CommandInvocationId::new(Ulid::new()),
            command_id: String::from("kraai-open-files"),
            deltas: vec![ContextStateDelta {
                namespace: String::from("opened_files"),
                operation: String::from("open"),
                payload: serde_json::json!({ "path": path }),
            }],
        }
    }

    #[test]
    fn open_file_scope_is_derived_from_the_actual_path_and_execution_authority() {
        let workspace = Path::new("/workspace");
        let workspace_read = SandboxCapabilities::workspace_read();
        let workspace_mutations = authorize_context_mutations(
            &open_request("/workspace/src/lib.rs"),
            workspace,
            &workspace_read,
        )
        .unwrap();
        assert!(matches!(
            workspace_mutations.first(),
            Some(ContextStateMutation::PinFile {
                scope: PinnedFileScope::Workspace { root },
                ..
            }) if root == workspace
        ));

        let denied = authorize_context_mutations(
            &open_request("/host/file.txt"),
            workspace,
            &workspace_read,
        )
        .unwrap_err();
        assert!(denied.contains("without host-read"));

        let host_read = SandboxCapabilities::new([SandboxCapability::HostRead]).unwrap();
        let host_mutations =
            authorize_context_mutations(&open_request("/host/file.txt"), workspace, &host_read)
                .unwrap();
        assert!(matches!(
            host_mutations.first(),
            Some(ContextStateMutation::PinFile {
                scope: PinnedFileScope::Host,
                ..
            })
        ));
    }
}
