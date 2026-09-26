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
        Self(
            path.canonicalize()
                .unwrap_or_else(|error| panic!("unable to resolve test workspace: {error}")),
        )
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

#[cfg(target_os = "linux")]
fn inherited_path() -> String {
    std::env::var("PATH").unwrap_or_else(|_| String::from("/usr/bin:/bin"))
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

struct StalledEffects {
    entered: CancellationToken,
    dropped: CancellationToken,
}

impl StateEffectHandler for StalledEffects {
    fn apply<'a>(
        &'a self,
        _request: &'a StateEffectRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            let _dropped = self.dropped.clone().drop_guard();
            self.entered.cancel();
            std::future::pending().await
        })
    }
}

#[tokio::test]
async fn dropping_execution_cancels_an_in_progress_state_effect()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let workspace = TestWorkspace::new();
    std::fs::write(workspace.0.join("fixture.txt"), "fixture")?;
    let entered = CancellationToken::new();
    let dropped = CancellationToken::new();
    let mut plan = plan(b"kraai-open-files fixture.txt".to_vec(), &workspace);
    plan.active_commands.push(String::from("kraai-open-files"));
    plan.state_effect_handler = Arc::new(StalledEffects {
        entered: entered.clone(),
        dropped: dropped.clone(),
    });
    let mut execution = Box::pin(execute(plan, CancellationToken::new()));
    tokio::select! {
        result = &mut execution => {
            return Err(format!("execution stopped before entering the state effect: {result:?}").into());
        }
        entered = tokio::time::timeout(Duration::from_secs(5), entered.cancelled()) => entered?,
    }

    drop(execution);
    tokio::time::timeout(Duration::from_secs(5), dropped.cancelled()).await?;
    Ok(())
}

async fn stalled_effect_after_host_exit(
    exit_code: i32,
    cancel: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let workspace = TestWorkspace::new();
    std::fs::write(workspace.0.join("fixture.txt"), "fixture")?;
    let entered = CancellationToken::new();
    let dropped = CancellationToken::new();
    let mut plan = plan(
        format!(
            "print 'before effect'; print --stderr 'effect pending'; \
             job spawn {{ kraai-open-files fixture.txt }} | ignore; \
             while not ('exit-now' | path exists) {{ sleep 1ms }}; exit {exit_code}"
        )
        .into_bytes(),
        &workspace,
    );
    plan.timeout = Duration::from_secs(3);
    plan.active_commands.push(String::from("kraai-open-files"));
    plan.state_effect_handler = Arc::new(StalledEffects {
        entered: entered.clone(),
        dropped: dropped.clone(),
    });
    let cancellation = CancellationToken::new();
    let mut execution = Box::pin(execute(plan, cancellation.clone()));
    tokio::select! {
        result = &mut execution => {
            return Err(format!("execution stopped before entering the state effect: {result:?}").into());
        }
        entered = tokio::time::timeout(Duration::from_secs(5), entered.cancelled()) => entered?,
    }
    std::fs::write(workspace.0.join("exit-now"), "")?;
    if cancel {
        cancellation.cancel();
    }
    let completed = tokio::time::timeout(Duration::from_secs(5), &mut execution).await;
    drop(execution);
    tokio::time::timeout(Duration::from_secs(1), dropped.cancelled()).await?;
    let result =
        completed.map_err(|_elapsed| "execution ignored script timeout after host exit")??;
    assert_eq!(
        result.output.termination,
        if cancel {
            Termination::Cancelled
        } else {
            Termination::TimedOut
        }
    );
    assert_eq!(result.output.stdout, b"before effect\n");
    assert_eq!(result.output.stderr, b"effect pending\n");
    Ok(())
}

#[tokio::test]
async fn script_timeout_still_bounds_effect_completion_after_host_exit()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for exit_code in [0, 7] {
        stalled_effect_after_host_exit(exit_code, false).await?;
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_drops_a_pending_state_effect()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    stalled_effect_after_host_exit(0, true).await
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "test asserts behavior and propagates fixture errors"
)]
async fn completed_state_effects_preserve_the_host_exit_status()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for exit_code in [0, 7] {
        let workspace = TestWorkspace::new();
        std::fs::write(workspace.0.join("fixture.txt"), "fixture")?;
        let effects = Arc::new(RecordingEffects::default());
        let mut plan = plan(
            format!(
                "kraai-open-files fixture.txt | ignore; print 'after effect'; exit {exit_code}"
            )
            .into_bytes(),
            &workspace,
        );
        plan.active_commands.push(String::from("kraai-open-files"));
        plan.state_effect_handler = effects.clone();

        let result = execute(plan, CancellationToken::new()).await?;
        assert_eq!(
            result.output.termination,
            Termination::Exited {
                code: Some(exit_code)
            }
        );
        assert_eq!(result.output.stdout, b"after effect\n");
        assert!(result.output.stderr.is_empty());
        assert_eq!(effects.requests.lock().expect("recording lock").len(), 1);
    }
    Ok(())
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
async fn timeout_after_a_successful_handshake_remains_a_script_timeout() {
    let workspace = TestWorkspace::new();
    let mut execution = plan(b"sleep 10sec".to_vec(), &workspace);
    execution.timeout = Duration::from_secs(1);
    let result = execute(execution, CancellationToken::new())
        .await
        .expect("a running script timing out is not a host failure");
    assert_eq!(result.output.termination, Termination::TimedOut);
}

#[tokio::test]
async fn exposes_print_from_the_nushell_cli_context() {
    let workspace = TestWorkspace::new();
    let result = execute(
        plan(
            b"print 'stdout-value'; print --stderr 'stderr-value'".to_vec(),
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
    assert_eq!(
        String::from_utf8_lossy(&result.output.stdout),
        "stdout-value\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&result.output.stderr),
        "stderr-value\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn sandboxed_host_accepts_a_workspace_symlink_alias() {
    let workspace = TestWorkspace::new();
    let aliases = TestWorkspace::new();
    let alias = aliases.0.join("workspace");
    std::os::unix::fs::symlink(&workspace.0, &alias).expect("create workspace alias");
    for capability in [
        SandboxCapability::WorkspaceRead,
        SandboxCapability::WorkspaceWrite,
    ] {
        let mut execution = plan(b"$env.PWD".to_vec(), &workspace);
        execution.workspace_root = alias.clone();
        execution.capabilities = SandboxCapabilities::new([capability]).expect("capabilities");
        execution.runtime_roots = sandbox_runtime_roots(host_executable());
        let result = execute(execution, CancellationToken::new())
            .await
            .expect("launch aliased workspace");
        assert_eq!(
            result.output.termination,
            Termination::Exited { code: Some(0) },
            "{}",
            String::from_utf8_lossy(&result.output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&result.output.stdout).trim(),
            workspace.0.to_string_lossy()
        );
    }
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
    let mut execution = plan(b"'unreachable'".to_vec(), &workspace);
    execution
        .host_arguments
        .push("--invalid-host-argument".into());

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        execute(execution, CancellationToken::new()),
    )
    .await
    .expect("execution should not hang");
    assert!(
        matches!(&result, Err(RuntimeError::Transport(_))),
        "unexpected execution result: {result:?}"
    );
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn transport_descriptor_is_closed_before_external_commands_can_run() {
    let workspace = TestWorkspace::new();
    let mut execution = plan(
        br#"^sh -c 'test ! -e /dev/fd/20 && printf "closed\n"'"#.to_vec(),
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sandbox_runtime_roots(host: PathBuf) -> Vec<PathBuf> {
    let mut roots = vec![host];
    for path in ["/nix/store", "/lib", "/lib64", "/usr/lib"] {
        let path = PathBuf::from(path);
        if path.exists() {
            roots.push(path);
        }
    }
    roots
}

#[tokio::test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn private_transport_crosses_the_sandbox_boundary() {
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
    execution.runtime_roots = sandbox_runtime_roots(host);

    let result = match execute(execution, CancellationToken::new()).await {
        Ok(result) => result,
        #[cfg(target_os = "linux")]
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
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
    execution.runtime_roots = sandbox_runtime_roots(host);

    let result = match execute(execution, CancellationToken::new()).await {
        Ok(result) => result,
        #[cfg(target_os = "linux")]
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

#[cfg(windows)]
#[tokio::test]
async fn inherited_pipe_delivers_authenticated_effects_without_network_access() {
    let workspace = TestWorkspace::new();
    std::fs::write(workspace.0.join("notes.txt"), "sandboxed context")
        .unwrap_or_else(|error| panic!("unable to write fixture: {error}"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|error| panic!("unable to bind network fixture: {error}"));
    listener
        .set_nonblocking(true)
        .unwrap_or_else(|error| panic!("unable to configure network fixture: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("unable to get network fixture address: {error}"));
    let effects = Arc::new(RecordingEffects::default());
    let mut execution = plan(
        format!(
            "kraai-open-files notes.txt | ignore; \
             try {{ http get --max-time 1sec http://{address} | ignore }} catch {{}}; \
             'effect-acknowledged'"
        )
        .into_bytes(),
        &workspace,
    );
    execution.capabilities = SandboxCapabilities::new([SandboxCapability::WorkspaceRead])
        .unwrap_or_else(|error| panic!("invalid test capabilities: {error}"));
    let runtime = TestWorkspace::new();
    let host = runtime.0.join("kraai-nushell-host.exe");
    std::fs::copy(host_executable(), &host).expect("copy host without Cargo's hard links");
    execution.host_executable = host.clone();
    execution.runtime_roots.push(host);
    execution.active_commands = vec![String::from("kraai-open-files")];
    execution.state_effect_handler = effects.clone();

    let result = execute(execution, CancellationToken::new())
        .await
        .unwrap_or_else(|error| panic!("sandboxed host execution failed: {error}"));
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) },
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&result.output.stdout),
        "effect-acknowledged\n"
    );
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
    let requests = effects
        .requests
        .lock()
        .unwrap_or_else(|error| panic!("recording lock failed: {error}"));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].command_id, "kraai-open-files");
    drop(requests);
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "test asserts behavior and propagates fixture errors"
)]
async fn skill_reads_return_text_without_context_effects() -> Result<(), Box<dyn std::error::Error>>
{
    let workspace = TestWorkspace::new();
    let directory = workspace.0.join(".agents/skills/review");
    std::fs::create_dir_all(&directory)?;
    let instructions = "---\nname: review\ndescription: Review code\n---\nInspect the changes.\n";
    std::fs::write(directory.join("SKILL.md"), instructions)?;
    let effects = Arc::new(RecordingEffects::default());
    let mut execution = plan(
        b"open --raw .agents/skills/review/SKILL.md".to_vec(),
        &workspace,
    );
    execution.state_effect_handler = effects.clone();
    let result = execute(execution, CancellationToken::new()).await?;
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) }
    );
    assert!(String::from_utf8_lossy(&result.output.stdout).contains(instructions));
    assert!(
        effects
            .requests
            .lock()
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    Ok(())
}

#[path = "host_execution/web_search.rs"]
mod web_search;

#[tokio::test]
async fn startup_errors_fail_inherited_execution_but_do_not_affect_clean_execution() {
    for (filename, source) in [
        ("env.nu", "do --ignore-shell-errors { 'ignored' }"),
        ("config.nu", "$env.config.footer_mode = '25'"),
    ] {
        let workspace = TestWorkspace::new();
        let config_home = workspace.0.join("config");
        let nushell_config = config_home.join("nushell");
        std::fs::create_dir_all(&nushell_config).expect("create config directory");
        std::fs::write(nushell_config.join(filename), source).expect("write invalid startup file");
        for startup in [NushellStartup::Inherit, NushellStartup::Clean] {
            let mut execution = plan(b"print 'script-ran'".to_vec(), &workspace);
            execution.nushell_startup = startup;
            execution
                .environment
                .insert("XDG_CONFIG_HOME".into(), config_home.display().to_string());
            let result = execute(execution, CancellationToken::new())
                .await
                .expect("run host");
            if startup == NushellStartup::Inherit {
                assert_eq!(
                    result.output.termination,
                    Termination::Exited { code: Some(70) }
                );
                assert!(!String::from_utf8_lossy(&result.output.stdout).contains("script-ran"));
                assert!(String::from_utf8_lossy(&result.output.stderr).contains(filename));
            } else {
                assert_eq!(
                    result.output.termination,
                    Termination::Exited { code: Some(0) }
                );
                assert_eq!(
                    String::from_utf8_lossy(&result.output.stdout),
                    "script-ran\n"
                );
                assert!(result.output.stderr.is_empty());
            }
        }
    }
}
#[path = "host_execution/images.rs"]
mod images;
