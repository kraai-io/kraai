#[cfg(target_os = "macos")]
pub(crate) mod members;
mod profile;

#[cfg(target_os = "macos")]
pub(crate) async fn prepare(
    plan: crate::config::LaunchPlan,
    private_temp: crate::temp_dir::PrivateTempDir,
) -> Result<crate::config::PreparedCommand, crate::SandboxError> {
    const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

    let metadata = tokio::fs::metadata(SANDBOX_EXEC).await.map_err(|error| {
        crate::SandboxError::SandboxUnavailable(format!("cannot access {SANDBOX_EXEC}: {error}"))
    })?;
    if !metadata.is_file() {
        return Err(crate::SandboxError::SandboxUnavailable(format!(
            "{SANDBOX_EXEC} is not a file"
        )));
    }
    let seatbelt = profile::build_args(&plan, private_temp.path())?;
    let mut environment = plan.environment;
    private_temp.apply_environment(&mut environment);

    Ok(crate::config::PreparedCommand {
        executable: SANDBOX_EXEC.into(),
        args: seatbelt.args,
        cwd: plan.workspace_root,
        environment,
        sandboxed: true,
        output_events: plan.output_events,
        private_temp: Some(private_temp),
        run_marker: Some(seatbelt.run_marker),
    })
}

#[cfg(test)]
mod tests;
