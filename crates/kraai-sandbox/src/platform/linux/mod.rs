mod bubblewrap;
mod seccomp;

pub use seccomp::restrict_network_after_startup;

use crate::config::{LaunchPlan, PreparedCommand};
use crate::error::SandboxError;
use crate::temp_dir::PrivateTempDir;

#[cfg(test)]
pub(crate) use bubblewrap::{
    BWRAP_SECCOMP_STDIN_FD, build_bwrap_args, build_bwrap_probe_args, bwrap_probe_failure_message,
    find_bwrap, run_bwrap_sandbox_probe,
};
#[cfg(test)]
pub(crate) use seccomp::{
    SeccompInstruction, install_restricted_network_filter, restricted_network_seccomp_program,
};

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
    let args = bubblewrap::build_bwrap_args(&plan, private_temp.path())?;
    let mut environment = plan.environment;
    if let Some(path) = environment.get_mut(std::ffi::OsStr::new("PATH")) {
        *path = resolve_search_path(path);
    }
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

pub(crate) fn resolve_search_path(path: &std::ffi::OsStr) -> std::ffi::OsString {
    let entries = std::env::split_paths(path).flat_map(|entry| {
        let resolved = entry
            .is_absolute()
            .then(|| entry.canonicalize().ok())
            .flatten()
            .filter(|resolved| resolved != &entry)
            .filter(|resolved| std::env::join_paths([resolved]).is_ok());
        std::iter::once(entry).chain(resolved)
    });
    std::env::join_paths(entries).unwrap_or_else(|_| path.to_os_string())
}
