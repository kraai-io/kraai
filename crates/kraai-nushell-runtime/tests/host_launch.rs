// Clippy only applies its allow-*-in-tests settings to helpers under a `cfg(test)` ancestor.
#![cfg(test)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use kraai_execution_types::{SandboxCapabilities, SandboxCapability, ScriptExecutionId};
use kraai_nushell_runtime::{RuntimeError, ScriptExecutionPlan, execute};
use kraai_sandbox::{PrivateTempConfig, SandboxError};
use kraai_test_support::{HANG_TIMEOUT, hang_guard};
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

async fn unavailable_host(mode: u32) {
    let directory = PrivateTempConfig::default().reserve().unwrap();
    let workspace = directory.path().expect("missing private temp");
    let host = workspace.join("kraai-nushell-host");
    std::fs::write(&host, "invalid executable\n").unwrap();
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(mode)).unwrap();
    let mut plan = ScriptExecutionPlan::new(
        ScriptExecutionId::new(Ulid::generate()),
        host,
        b"'must not run'".to_vec(),
        workspace.to_path_buf(),
        SandboxCapabilities::new([SandboxCapability::NoSandbox]),
        HANG_TIMEOUT * 2,
    );
    plan.startup_timeout = HANG_TIMEOUT * 2;
    let result = hang_guard(execute(plan, CancellationToken::new()))
        .await
        .unwrap();
    let unavailable = match &result {
        Err(RuntimeError::Sandbox(SandboxError::Spawn { .. })) => true,
        Err(RuntimeError::Transport(_)) => cfg!(target_os = "macos") && mode == 0o700,
        _ => false,
    };
    if !unavailable {
        panic!("expected an unavailable host, received {result:?}");
    }
}

#[tokio::test]
async fn host_without_execute_permission_fails_to_launch() {
    unavailable_host(0o600).await
}

#[tokio::test]
async fn host_with_invalid_executable_format_is_unavailable() {
    unavailable_host(0o700).await
}
