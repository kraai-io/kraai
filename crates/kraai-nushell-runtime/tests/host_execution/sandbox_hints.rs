use super::*;
use kraai_execution_types::SandboxHint;
use kraai_nushell_runtime::ScriptExecutionResult;

async fn sandboxed(
    script: &str,
    workspace: &TestWorkspace,
    capabilities: &[SandboxCapability],
) -> Option<ScriptExecutionResult> {
    let mut execution = plan(script.as_bytes().to_vec(), workspace);
    execution.capabilities = SandboxCapabilities::new(capabilities.iter().copied());
    execution.active_commands = vec![String::from("kraai-edit-file")];
    execution.runtime_roots = sandbox_runtime_roots(host_executable());
    #[cfg(target_os = "linux")]
    execution
        .environment
        .insert(String::from("PATH"), inherited_path());
    match execute(execution, CancellationToken::new()).await {
        Ok(result) => Some(result),
        #[cfg(target_os = "linux")]
        Err(RuntimeError::Sandbox(kraai_sandbox::SandboxError::SandboxUnavailable(_))) => None,
        Err(error) => panic!("sandboxed host execution failed: {error}"),
    }
}

fn describe(result: &ScriptExecutionResult) -> String {
    format!(
        "{:?}\nstdout: {}\nstderr: {}",
        result.output.termination,
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr)
    )
}

#[tokio::test]
async fn kraai_edit_of_workspace_instructions_gets_the_protected_entry_hint() {
    let workspace = TestWorkspace::new();
    let instructions = workspace.0.join("AGENTS.md");
    std::fs::write(&instructions, "old\n").expect("write instructions");
    let Some(result) = sandboxed(
        "kraai-edit-file AGENTS.md [{start_line: 1, end_line: 1, old_text: \"old\\n\", new_text: \"new\\n\"}]",
        &workspace,
        &[SandboxCapability::WorkspaceWrite],
    )
    .await
    else {
        return;
    };
    assert_eq!(
        result.sandbox_hint,
        Some(SandboxHint::ProtectedEntry),
        "{}",
        describe(&result)
    );
    assert_eq!(
        std::fs::read_to_string(&instructions).expect("read instructions"),
        "old\n"
    );
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn external_write_to_the_kraai_directory_gets_the_protected_entry_hint() {
    let workspace = TestWorkspace::new();
    std::fs::create_dir(workspace.0.join(".kraai")).expect("create .kraai");
    std::fs::write(workspace.0.join(".kraai/agents.toml"), "").expect("write agents.toml");
    let Some(result) = sandboxed(
        "^sh -c 'echo x > .kraai/other.toml'",
        &workspace,
        &[
            SandboxCapability::HostRead,
            SandboxCapability::WorkspaceWrite,
        ],
    )
    .await
    else {
        return;
    };
    assert_eq!(
        result.sandbox_hint,
        Some(SandboxHint::ProtectedEntry),
        "{}",
        describe(&result)
    );
    assert!(!workspace.0.join(".kraai/other.toml").exists());
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn created_instructions_are_undone_and_get_a_hint() {
    let workspace = TestWorkspace::new();
    let Some(result) = sandboxed(
        "'planted' | save AGENTS.md",
        &workspace,
        &[SandboxCapability::WorkspaceWrite],
    )
    .await
    else {
        return;
    };
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) }
    );
    assert_eq!(
        result.sandbox_hint,
        Some(SandboxHint::ProtectedEntryUndone),
        "{}",
        describe(&result)
    );
    assert!(std::fs::symlink_metadata(workspace.0.join("AGENTS.md")).is_err());
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn host_socket_failure_gets_the_host_socket_hint() {
    let workspace = TestWorkspace::new();
    let socket_dir = TestWorkspace::new();
    let socket = socket_dir.0.join("daemon.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("host socket");
    let Some(result) = sandboxed(
        &format!(
            "http get --unix-socket '{}' http://localhost/",
            socket.display()
        ),
        &workspace,
        &[
            SandboxCapability::WorkspaceRead,
            SandboxCapability::HostRead,
            SandboxCapability::Network,
        ],
    )
    .await
    else {
        return;
    };
    assert_eq!(
        result.sandbox_hint,
        Some(SandboxHint::HostSocket),
        "{}",
        describe(&result)
    );
}

#[tokio::test]
async fn unrelated_sandboxed_failures_get_no_hint() {
    let workspace = TestWorkspace::new();
    std::fs::write(workspace.0.join("AGENTS.md"), "instructions\n").expect("write instructions");
    for (script, capabilities) in [
        ("exit 3", &[SandboxCapability::WorkspaceWrite][..]),
        (
            "open AGENTS.md; 'x' | save notes.txt",
            &[SandboxCapability::WorkspaceRead][..],
        ),
        (
            "open AGENTS.md; 'x' | save notes.txt",
            &[SandboxCapability::WorkspaceWrite][..],
        ),
    ] {
        let Some(result) = sandboxed(script, &workspace, capabilities).await else {
            return;
        };
        assert_eq!(result.sandbox_hint, None, "{script}: {}", describe(&result));
    }
}
