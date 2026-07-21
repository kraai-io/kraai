mod bubblewrap;
mod seccomp;

use crate::config::{LaunchPlan, PreparedCommand};
use crate::error::SandboxError;
use crate::temp_dir::PrivateTempDir;

#[cfg(test)]
pub(crate) use bubblewrap::{
    BWRAP_SECCOMP_STDIN_FD, build_bwrap_args, build_bwrap_probe_args, bwrap_probe_failure_message,
    find_bwrap, run_bwrap_sandbox_probe,
};
#[cfg(test)]
pub(crate) use seccomp::{SeccompInstruction, restricted_network_seccomp_program};

pub(crate) async fn prepare(
    plan: LaunchPlan,
    private_temp: PrivateTempDir,
) -> Result<PreparedCommand, SandboxError> {
    let network_enabled = plan
        .capabilities
        .contains(kraai_types::SandboxCapability::Network);
    let bwrap =
        bubblewrap::find_bwrap(&plan.workspace_root, private_temp.path()).ok_or_else(|| {
            SandboxError::SandboxUnavailable(String::from(
                "bubblewrap was not found on a trusted PATH entry",
            ))
        })?;
    bubblewrap::ensure_bwrap_sandbox_available(&bwrap, network_enabled).await?;
    let seccomp_filter = seccomp::restricted_network_seccomp_filter(
        network_enabled,
        &plan.private_ipc_connect_descriptors,
    )?;
    let args = bubblewrap::build_bwrap_args(&plan, private_temp.path());
    let mut environment = plan.environment;
    private_temp.apply_environment(&mut environment);

    Ok(PreparedCommand {
        executable: bwrap,
        args,
        cwd: plan.workspace_root,
        environment,
        sandboxed: true,
        output_events: plan.output_events,
        private_temp,
        seccomp_filter,
    })
}
