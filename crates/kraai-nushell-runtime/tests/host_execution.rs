#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration tests use direct fixture failures and structural assertions"
)]

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kraai_nushell_runtime::{RuntimeError, ScriptExecutionPlan, StateEffectHandler, execute};
use kraai_sandbox::Termination;
use kraai_types::{NushellStartup, SandboxCapabilities, SandboxCapability, StateEffectRequest};
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

fn host_executable() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kraai-nushell-host"))
}

struct TestWorkspace(PathBuf);

impl TestWorkspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("kraai-nu-host-{}", Ulid::generate()));
        std::fs::create_dir(&path)
            .unwrap_or_else(|error| panic!("unable to create test workspace: {error}"));
        Self(path)
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn plan(source: impl Into<Vec<u8>>, workspace: &TestWorkspace) -> ScriptExecutionPlan {
    let capabilities = SandboxCapabilities::new([SandboxCapability::NoSandbox])
        .unwrap_or_else(|error| panic!("invalid test capabilities: {error}"));
    let mut plan = ScriptExecutionPlan::new(
        kraai_types::ScriptExecutionId::new(Ulid::generate()),
        host_executable(),
        source.into(),
        workspace.0.clone(),
        capabilities,
        Duration::from_secs(30),
    );
    plan.environment
        .insert(String::from("TERM"), String::from("dumb"));
    plan
}

fn inherited_path() -> String {
    std::env::var("PATH").unwrap_or_else(|_| String::from("/usr/bin:/bin"))
}

fn external_executable(name: &str) -> PathBuf {
    std::env::split_paths(&std::ffi::OsString::from(inherited_path()))
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("unable to find test executable '{name}'"))
}

#[derive(Default)]
struct RecordingEffects {
    requests: Mutex<Vec<StateEffectRequest>>,
}

impl StateEffectHandler for RecordingEffects {
    fn apply<'a>(
        &'a self,
        request: &'a StateEffectRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            self.requests
                .lock()
                .map_err(|error| format!("recording lock failed: {error}"))?
                .push(request.clone());
            Ok(())
        })
    }
}

#[tokio::test]
async fn executes_structured_nushell_through_the_private_transport() {
    let workspace = TestWorkspace::new();
    let result = execute(
        plan(
            b"[1 2 3] | each {|number| $number * 2 } | to json --raw".to_vec(),
            &workspace,
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|error| panic!("host execution failed: {error}"));

    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) },
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&result.output.stdout), "[2,4,6]\n");
    assert!(result.output.stderr.is_empty());
}

#[tokio::test]
async fn invalid_source_is_reported_by_nushell_without_partial_evaluation() {
    let workspace = TestWorkspace::new();
    let result = execute(
        plan(b"touch should-not-exist; let =".to_vec(), &workspace),
        CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|error| panic!("host execution failed: {error}"));

    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(1) }
    );
    assert!(!workspace.0.join("should-not-exist").exists());
    assert!(!result.output.stderr.is_empty());
}

#[tokio::test]
async fn inherited_startup_evaluates_env_and_config_files_before_the_script() {
    let workspace = TestWorkspace::new();
    let config_home = workspace.0.join("config");
    let nushell_config = config_home.join("nushell");
    std::fs::create_dir_all(&nushell_config)
        .unwrap_or_else(|error| panic!("unable to create Nushell config fixture: {error}"));
    std::fs::write(
        nushell_config.join("env.nu"),
        "$env.KRAAI_ENV_STARTUP = 'env-loaded'\n",
    )
    .unwrap_or_else(|error| panic!("unable to write env.nu fixture: {error}"));
    std::fs::write(
        nushell_config.join("config.nu"),
        "$env.KRAAI_CONFIG_STARTUP = 'config-loaded'\n",
    )
    .unwrap_or_else(|error| panic!("unable to write config.nu fixture: {error}"));

    let mut execution = plan(
        b"[$env.KRAAI_ENV_STARTUP $env.KRAAI_CONFIG_STARTUP] | to json --raw".to_vec(),
        &workspace,
    );
    execution.nushell_startup = NushellStartup::Inherit;
    execution.environment.insert(
        String::from("XDG_CONFIG_HOME"),
        config_home.display().to_string(),
    );

    let result = execute(execution, CancellationToken::new())
        .await
        .unwrap_or_else(|error| panic!("host execution failed: {error}"));
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) },
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&result.output.stdout),
        "[\"env-loaded\",\"config-loaded\"]\n"
    );
}

#[tokio::test]
async fn a_host_that_exits_without_connecting_fails_without_waiting_forever() {
    let workspace = TestWorkspace::new();
    let capabilities = SandboxCapabilities::new([SandboxCapability::NoSandbox])
        .unwrap_or_else(|error| panic!("invalid test capabilities: {error}"));
    let execution = ScriptExecutionPlan::new(
        kraai_types::ScriptExecutionId::new(Ulid::generate()),
        external_executable("true"),
        b"'unreachable'".to_vec(),
        workspace.0.clone(),
        capabilities,
        Duration::from_secs(30),
    );

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        execute(execution, CancellationToken::new()),
    )
    .await
    .expect("execution should not hang");
    assert!(
        matches!(&result, Err(RuntimeError::Transport(message)) if message.contains("before connecting")),
        "unexpected execution result: {result:?}"
    );
}

#[tokio::test]
async fn transport_descriptor_is_closed_before_external_commands_can_run() {
    let workspace = TestWorkspace::new();
    let mut execution = plan(
        br#"^sh -c 'test ! -e /proc/self/fd/20'; "closed""#.to_vec(),
        &workspace,
    );
    execution
        .environment
        .insert(String::from("PATH"), inherited_path());
    let result = execute(execution, CancellationToken::new())
        .await
        .unwrap_or_else(|error| panic!("host execution failed: {error}"));

    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) }
    );
    assert_eq!(String::from_utf8_lossy(&result.output.stdout), "closed\n");
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn private_transport_crosses_the_bubblewrap_boundary() {
    let workspace = TestWorkspace::new();
    let capabilities = SandboxCapabilities::new([SandboxCapability::WorkspaceRead])
        .unwrap_or_else(|error| panic!("invalid test capabilities: {error}"));
    let host = host_executable();
    let mut execution = ScriptExecutionPlan::new(
        kraai_types::ScriptExecutionId::new(Ulid::generate()),
        host.clone(),
        b"{transport: private, engine: embedded} | to json --raw".to_vec(),
        workspace.0.clone(),
        capabilities,
        Duration::from_secs(30),
    );
    execution
        .environment
        .insert(String::from("TERM"), String::from("dumb"));
    execution.runtime_roots.push(
        host.parent()
            .unwrap_or_else(|| panic!("host executable has no parent"))
            .to_path_buf(),
    );
    let nix_store = PathBuf::from("/nix/store");
    if nix_store.exists() {
        execution.runtime_roots.push(nix_store);
    }

    let result = match execute(execution, CancellationToken::new()).await {
        Ok(result) => result,
        Err(RuntimeError::Sandbox(kraai_sandbox::SandboxError::SandboxUnavailable(_))) => return,
        Err(error) => panic!("sandboxed host execution failed: {error}"),
    };
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) },
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&result.output.stdout),
        "{\"transport\":\"private\",\"engine\":\"embedded\"}\n"
    );
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn native_commands_remain_registered_when_the_sandbox_denies_the_operation() {
    let workspace = TestWorkspace::new();
    let capabilities = SandboxCapabilities::new([SandboxCapability::WorkspaceRead])
        .unwrap_or_else(|error| panic!("invalid test capabilities: {error}"));
    let host = host_executable();
    let mut execution = ScriptExecutionPlan::new(
        kraai_types::ScriptExecutionId::new(Ulid::generate()),
        host.clone(),
        b"kraai-edit-file denied.txt --create --contents 'denied'".to_vec(),
        workspace.0.clone(),
        capabilities,
        Duration::from_secs(30),
    );
    execution.active_commands = vec![String::from("kraai-edit-file")];
    execution
        .environment
        .insert(String::from("TERM"), String::from("dumb"));
    execution.runtime_roots.push(
        host.parent()
            .unwrap_or_else(|| panic!("host executable has no parent"))
            .to_path_buf(),
    );
    let nix_store = PathBuf::from("/nix/store");
    if nix_store.exists() {
        execution.runtime_roots.push(nix_store);
    }

    let result = match execute(execution, CancellationToken::new()).await {
        Ok(result) => result,
        Err(RuntimeError::Sandbox(kraai_sandbox::SandboxError::SandboxUnavailable(_))) => return,
        Err(error) => panic!("sandboxed host execution failed: {error}"),
    };
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(1) },
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.output.stderr);
    assert!(stderr.contains("Unable to create file"), "stderr: {stderr}");
    assert!(!stderr.contains("invalid active command set"));
    assert!(!workspace.0.join("denied.txt").exists());
}

#[tokio::test]
async fn native_open_files_waits_for_an_authenticated_state_effect_ack() {
    let workspace = TestWorkspace::new();
    std::fs::write(workspace.0.join("notes.txt"), "fresh context")
        .unwrap_or_else(|error| panic!("unable to write fixture: {error}"));
    let effects = Arc::new(RecordingEffects::default());
    let mut execution = plan(
        b"kraai-open-files notes.txt | to json --raw".to_vec(),
        &workspace,
    );
    execution.active_commands = vec![String::from("kraai-open-files")];
    execution.state_effect_handler = effects.clone();

    let result = execute(execution, CancellationToken::new())
        .await
        .unwrap_or_else(|error| panic!("host execution failed: {error}"));
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) }
    );
    let output: serde_json::Value = serde_json::from_slice(&result.output.stdout)
        .unwrap_or_else(|error| panic!("invalid command output: {error}"));
    assert_eq!(output["success"], true);
    assert_eq!(
        output["paths"][0],
        workspace.0.join("notes.txt").display().to_string()
    );

    let requests = effects
        .requests
        .lock()
        .unwrap_or_else(|error| panic!("recording lock failed: {error}"));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].command_id, "kraai-open-files");
    assert_eq!(requests[0].deltas.len(), 1);
    assert_eq!(requests[0].deltas[0].namespace, "opened_files");
    assert_eq!(requests[0].deltas[0].operation, "open");
    drop(requests);
}

#[tokio::test]
async fn stateful_commands_ack_each_completed_effect_in_script_order() {
    let workspace = TestWorkspace::new();
    std::fs::write(workspace.0.join("notes.txt"), "fresh context")
        .unwrap_or_else(|error| panic!("unable to write fixture: {error}"));
    let effects = Arc::new(RecordingEffects::default());
    let mut execution = plan(
        b"kraai-open-files notes.txt; kraai-close-files notes.txt".to_vec(),
        &workspace,
    );
    execution.active_commands = vec![
        String::from("kraai-open-files"),
        String::from("kraai-close-files"),
    ];
    execution.state_effect_handler = effects.clone();

    let result = execute(execution, CancellationToken::new())
        .await
        .unwrap_or_else(|error| panic!("host execution failed: {error}"));
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) }
    );
    let requests = effects
        .requests
        .lock()
        .unwrap_or_else(|error| panic!("recording lock failed: {error}"));
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].deltas[0].operation, "open");
    assert_eq!(requests[1].deltas[0].operation, "close");
    drop(requests);
}
