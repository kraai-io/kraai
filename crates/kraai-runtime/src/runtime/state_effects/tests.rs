use super::*;
mod cancellation;
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

    let denied =
        authorize_context_mutations(&open_request("/host/file.txt"), workspace, &workspace_read)
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
