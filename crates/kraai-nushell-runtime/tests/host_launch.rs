#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use kraai_nushell_runtime::{RuntimeError, ScriptExecutionPlan, execute};
use kraai_sandbox::{PrivateTempConfig, SandboxError};
use kraai_types::{SandboxCapabilities, SandboxCapability, ScriptExecutionId};
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

async fn unlaunchable_host(mode: u32) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let directory = PrivateTempConfig::default().reserve()?;
    let workspace = directory.path().ok_or("missing private temp")?;
    let host = workspace.join("kraai-nushell-host");
    std::fs::write(&host, "invalid executable\n")?;
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(mode))?;
    let plan = ScriptExecutionPlan::new(
        ScriptExecutionId::new(Ulid::generate()),
        host,
        b"'must not run'".to_vec(),
        workspace.to_path_buf(),
        SandboxCapabilities::new([SandboxCapability::NoSandbox])?,
        Duration::from_secs(30),
    );
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        execute(plan, CancellationToken::new()),
    )
    .await?;
    if !matches!(
        &result,
        Err(RuntimeError::Sandbox(SandboxError::Spawn { .. }))
    ) {
        return Err(format!("expected a host launch failure, received {result:?}").into());
    }
    Ok(())
}

#[tokio::test]
async fn host_without_execute_permission_fails_to_launch()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    unlaunchable_host(0o600).await
}

#[tokio::test]
async fn host_with_invalid_executable_format_fails_to_launch()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    unlaunchable_host(0o700).await
}
