use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use kraai_nushell_runtime::StateEffectHandler;
use kraai_persistence::ContextStateStore;
use kraai_types::{
    ContextStateMutation, OpenedFilesOperation, PinnedFileScope, SandboxCapabilities,
    SandboxCapability, ScriptExecutionId, StateEffectRequest,
};

pub(super) struct DurableStateEffects {
    pub(super) execution_id: ScriptExecutionId,
    pub(super) session_id: String,
    pub(super) workspace_root: PathBuf,
    pub(super) effective_capabilities: SandboxCapabilities,
    pub(super) store: Arc<dyn ContextStateStore>,
    pub(super) barrier: Arc<tokio::sync::RwLock<()>>,
    pub(super) session_task: Arc<super::stream_tasks::SessionTaskToken>,
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
            let session_task = self.session_task.clone();
            let guard = self.barrier.clone().read_owned().await;
            let store = self.store.clone();
            let session_id = self.session_id.clone();
            let execution_id = self.execution_id.clone();
            let sequence = request.sequence;
            let invocation_id = request.invocation_id.clone();
            let command_id = request.command_id.clone();
            tokio::spawn(async move {
                let _session_task = session_task;
                let result = store
                    .append_command(
                        &session_id,
                        &execution_id,
                        sequence,
                        &invocation_id,
                        &command_id,
                        mutations,
                    )
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                drop(guard);
                result
            })
            .await
            .map_err(|error| format!("Context state commit task failed: {error}"))?
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
            if delta.namespace != OpenedFilesOperation::NAMESPACE {
                return Err(format!(
                    "command '{}' requested unsupported context namespace '{}'",
                    request.command_id, delta.namespace
                ));
            }
            let path = delta
                .opened_file_path()
                .map(PathBuf::from)
                .ok_or_else(|| String::from("opened-files mutation requires a string path"))?;
            if !path.is_absolute() {
                return Err(format!(
                    "opened-files mutation path must be absolute: {}",
                    path.display()
                ));
            }
            match (
                request.command_id.as_str(),
                OpenedFilesOperation::parse(&delta.operation),
            ) {
                (command_id, Some(OpenedFilesOperation::Open))
                    if command_id == kraai_command_catalog::OPEN_FILES.id =>
                {
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
                (command_id, Some(OpenedFilesOperation::Close))
                    if command_id == kraai_command_catalog::CLOSE_FILES.id =>
                {
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

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "context mutation authorization tests use direct fixture assertions"
)]
mod tests;
