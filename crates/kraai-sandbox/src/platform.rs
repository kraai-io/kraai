use crate::config::{LaunchPlan, PreparedCommand, path_is_absolute};
use crate::error::SandboxError;
use kraai_execution_types::SandboxCapability;

pub(crate) mod git_worktree;

#[cfg(target_os = "linux")]
pub(crate) mod linux;

#[cfg(windows)]
pub(crate) mod windows;

#[cfg(test)]
#[cfg(not(windows))]
#[path = "platform/windows/filesystem.rs"]
mod windows_filesystem;

#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) mod macos;

#[must_use]
pub fn availability_warning() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        linux::availability_warning()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

pub(crate) async fn prepare_command(mut plan: LaunchPlan) -> Result<PreparedCommand, SandboxError> {
    validate_plan(&plan)?;
    #[cfg(windows)]
    windows::apply_system_environment(&mut plan.environment)?;
    let private_temp = std::mem::take(&mut plan.private_temp).into_private_temp();
    #[cfg(windows)]
    let private_temp =
        private_temp.map_err(|error| error.with_workspace_root(&plan.workspace_root));
    let private_temp = private_temp?;

    if plan.capabilities.is_unsandboxed() {
        return Ok(PreparedCommand::unsandboxed(plan, private_temp));
    }

    #[cfg(target_os = "linux")]
    {
        linux::prepare(plan, private_temp).await
    }
    #[cfg(target_os = "macos")]
    {
        macos::prepare(plan, private_temp).await
    }
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(move || windows::prepare(plan, private_temp))
            .await
            .map_err(|error| SandboxError::SandboxUnavailable(error.to_string()))?
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Err(SandboxError::SandboxUnavailable(String::from(
            "sandboxed execution is only implemented on Linux, macOS, and Windows",
        )))
    }
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

/// Missing paths resolve through their nearest existing ancestor, so rules that match by
/// path still cover them once something creates them. Shallower paths come first.
pub(crate) fn denied_paths(plan: &LaunchPlan) -> Vec<std::path::PathBuf> {
    let mut paths = plan
        .denied_paths
        .iter()
        .filter_map(|path| {
            let path = std::path::absolute(path).ok()?;
            path.ancestors().find_map(|ancestor| {
                let missing = path.strip_prefix(ancestor).ok()?;
                let resolved = ancestor.canonicalize().ok()?;
                Some(if missing.as_os_str().is_empty() {
                    resolved
                } else {
                    resolved.join(missing)
                })
            })
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    paths.sort_by_key(|path| path.components().count());
    paths
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
