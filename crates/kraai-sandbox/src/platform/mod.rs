use crate::config::{LaunchPlan, PreparedCommand, path_is_absolute};
use crate::error::SandboxError;
use crate::temp_dir::PrivateTempDir;
use kraai_types::SandboxCapability;

#[cfg(target_os = "linux")]
pub(crate) mod linux;

pub(crate) async fn prepare_command(mut plan: LaunchPlan) -> Result<PreparedCommand, SandboxError> {
    validate_plan(&plan)?;
    let private_temp = std::mem::take(&mut plan.private_temp).into_private_temp()?;

    if plan.capabilities.is_unsandboxed() {
        return Ok(PreparedCommand::unsandboxed(plan, private_temp));
    }

    prepare_sandboxed_command(plan, private_temp).await
}

fn validate_plan(plan: &LaunchPlan) -> Result<(), SandboxError> {
    if !path_is_absolute(&plan.executable) {
        return Err(SandboxError::ExecutableMustBeAbsolute);
    }
    if plan.timeout.is_zero() {
        return Err(SandboxError::InvalidTimeout);
    }
    if !plan.capabilities.is_unsandboxed()
        && !plan.capabilities.contains(SandboxCapability::WorkspaceRead)
    {
        return Err(SandboxError::WorkspaceReadRequired);
    }
    if !plan.workspace_root.is_absolute() || !plan.workspace_root.is_dir() {
        return Err(SandboxError::MissingWorkspace(format!(
            "'{}' must be an existing absolute directory",
            plan.workspace_root.display()
        )));
    }
    for root in &plan.runtime_roots {
        if !root.is_absolute() || !root.exists() {
            return Err(SandboxError::InvalidRuntimeRoot(format!(
                "'{}' must be an existing absolute path",
                root.display()
            )));
        }
    }
    if !plan.capabilities.is_unsandboxed()
        && !plan.capabilities.contains(SandboxCapability::HostRead)
        && !path_is_visible(&plan.executable, &plan.workspace_root, &plan.runtime_roots)
    {
        return Err(SandboxError::ExecutableNotVisible(
            plan.executable.display().to_string(),
        ));
    }
    Ok(())
}

fn path_is_visible(
    executable: &std::path::Path,
    workspace: &std::path::Path,
    roots: &[std::path::PathBuf],
) -> bool {
    let Ok(executable) = executable.canonicalize() else {
        return false;
    };
    std::iter::once(workspace)
        .chain(roots.iter().map(std::path::PathBuf::as_path))
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| executable.starts_with(root))
}

#[cfg(target_os = "linux")]
async fn prepare_sandboxed_command(
    plan: LaunchPlan,
    private_temp: PrivateTempDir,
) -> Result<PreparedCommand, SandboxError> {
    linux::prepare(plan, private_temp).await
}

#[cfg(not(target_os = "linux"))]
async fn prepare_sandboxed_command(
    _plan: LaunchPlan,
    _private_temp: PrivateTempDir,
) -> Result<PreparedCommand, SandboxError> {
    Err(SandboxError::SandboxUnavailable(String::from(
        "sandboxed execution is currently only implemented on Linux",
    )))
}
