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
mod tests {
    use super::*;
    use kraai_types::{CommandInvocationId, ContextStateDelta};
    use ulid::Ulid;

    fn open_request(path: &str) -> StateEffectRequest {
        StateEffectRequest {
            sequence: 1,
            invocation_id: CommandInvocationId::new(Ulid::generate()),
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

    #[test]
    fn context_effect_errors_preserve_validation_order() {
        for (namespace, payload, expected) in [
            (
                "unknown",
                serde_json::json!({ "path": false }),
                "command 'wrong-command' requested unsupported context namespace 'unknown'",
            ),
            (
                "opened_files",
                serde_json::json!({ "path": false }),
                "opened-files mutation requires a string path",
            ),
            (
                "opened_files",
                serde_json::json!({ "path": "relative" }),
                "opened-files mutation path must be absolute: relative",
            ),
            (
                "opened_files",
                serde_json::json!({ "path": "/host/file" }),
                "command 'wrong-command' cannot apply opened-files operation 'unknown'",
            ),
        ] {
            let request = StateEffectRequest {
                command_id: String::from("wrong-command"),
                deltas: vec![ContextStateDelta {
                    namespace: String::from(namespace),
                    operation: String::from("unknown"),
                    payload,
                }],
                ..open_request("/workspace/file")
            };
            assert_eq!(
                authorize_context_mutations(
                    &request,
                    Path::new("/workspace"),
                    &SandboxCapabilities::workspace_read(),
                )
                .unwrap_err(),
                expected,
            );
        }
    }

    #[test]
    fn close_file_effects_keep_extra_payload_fields_and_require_the_matching_command() {
        let delta = ContextStateDelta {
            namespace: String::from("opened_files"),
            operation: String::from("close"),
            payload: serde_json::json!({ "path": "/host/file", "extra": true }),
        };
        let mut request = StateEffectRequest {
            command_id: String::from("kraai-close-files"),
            deltas: vec![delta],
            ..open_request("/workspace/file")
        };
        let mutations = authorize_context_mutations(
            &request,
            Path::new("/workspace"),
            &SandboxCapabilities::workspace_read(),
        )
        .unwrap();
        assert!(matches!(
            mutations.first(),
            Some(ContextStateMutation::UnpinFile { path, reason: None })
                if path == Path::new("/host/file")
        ));

        request.command_id = String::from("kraai-open-files");
        assert_eq!(
            authorize_context_mutations(
                &request,
                Path::new("/workspace"),
                &SandboxCapabilities::workspace_read(),
            )
            .unwrap_err(),
            "command 'kraai-open-files' cannot apply opened-files operation 'close'",
        );
    }
}
