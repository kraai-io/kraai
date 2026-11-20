use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use kraai_execution_types::SandboxCapability;

use crate::SandboxError;
use crate::config::LaunchPlan;
use crate::config_guard::{self, ConfigGuard};
use crate::platform::git_worktree::linked_worktree_common_dir;

pub(super) struct SeatbeltCommand {
    pub(super) args: Vec<OsString>,
    pub(super) run_marker: String,
}

pub(super) fn build_args(
    plan: &LaunchPlan,
    private_temp: &Path,
) -> Result<SeatbeltCommand, SandboxError> {
    let workspace = resolve(&plan.workspace_root)?;
    let private_temp = resolve(private_temp)?;
    let mut profile = Profile::new();
    let run_marker = random_mach_name()?;
    let run_parameter = profile.named_parameter("KRAAI_RUN", &run_marker);
    profile.push(&format!(
        "(allow mach-lookup (global-name {run_parameter}))"
    ));
    let workspace_filter = profile.path_filter(&workspace, workspace.is_dir())?;
    let temp_filter = profile.path_filter(&private_temp, true)?;
    let git_dir = plan
        .capabilities
        .contains(SandboxCapability::WorkspaceWrite)
        .then(|| linked_worktree_common_dir(&workspace))
        .flatten();
    let git_filter = git_dir
        .as_deref()
        .map(|git_dir| profile.path_filter(git_dir, true))
        .transpose()?;
    let writable_exclusions = [
        Some(&workspace_filter),
        Some(&temp_filter),
        git_filter.as_ref(),
    ]
    .into_iter()
    .flatten()
    .map(|filter| format!("(require-not {filter})"))
    .collect::<Vec<_>>()
    .join(" ");

    if plan.capabilities.contains(SandboxCapability::HostRead) {
        profile.push("(allow file-read* file-map-executable)");
    } else {
        profile.push(&format!(
            "(allow file-read* file-map-executable {workspace_filter})"
        ));
    }
    profile.push(&format!(
        "(allow file-read* file-map-executable file-write* {temp_filter})"
    ));
    if plan.capabilities.contains(SandboxCapability::HostWrite) {
        profile.push("(allow file-write*)");
    } else if plan
        .capabilities
        .contains(SandboxCapability::WorkspaceWrite)
    {
        profile.push(&format!("(allow file-write* {workspace_filter})"));
    }
    if let Some(git_filter) = &git_filter {
        profile.push(&format!("(allow file-read* file-write* {git_filter})"));
    }

    let runtime_roots = plan
        .runtime_roots
        .iter()
        .map(|root| resolve(root))
        .collect::<Result<BTreeSet<_>, _>>()?;
    for root in runtime_roots {
        let filter = profile.path_filter(&root, root.is_dir())?;
        profile.push(&format!("(allow file-read* file-map-executable {filter})"));
        if !root.starts_with(&workspace) {
            profile.push(&format!(
                "(deny file-write* (require-all {filter} {writable_exclusions}))"
            ));
            profile.protect_ancestors(&root)?;
        }
    }

    if plan.capabilities.contains(SandboxCapability::Network) {
        profile.push(include_str!("network.sbpl"));
        let workspace_writable = plan
            .capabilities
            .contains(SandboxCapability::WorkspaceWrite)
            || plan.capabilities.contains(SandboxCapability::HostWrite);
        for directory in [
            Some(private_temp.as_path()),
            workspace_writable.then_some(workspace.as_path()),
            git_dir.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let parameter = profile.parameter(directory)?;
            profile.push(&format!(
                "(allow network-bind (local unix-socket (subpath {parameter})))"
            ));
            profile.push(&format!(
                "(allow network-outbound (remote unix-socket (subpath {parameter})))"
            ));
        }
    } else {
        for path in &plan.private_ipc_connect_paths {
            if !path.is_absolute() {
                return Err(unavailable(path, "private IPC path must be absolute"));
            }
            let path = resolve(path)?;
            if !path.starts_with(&private_temp) {
                return Err(unavailable(
                    &path,
                    "private IPC socket must be inside the private temporary directory",
                ));
            }
            let socket = profile.path_filter(&path, false)?;
            profile.push(&format!(
                "(allow network-outbound (remote unix-socket {socket}))"
            ));
        }
    }

    if ConfigGuard::applies_to(&plan.capabilities) {
        for entry in config_guard::entry_paths(&workspace) {
            let entry = profile.parameter(&entry)?;
            profile.push(&format!(
                "(deny file-write* (require-any (literal {entry}) (subpath {entry})))"
            ));
        }
        for path in config_guard::read_only_paths(&workspace) {
            let parameter = profile.parameter(&path)?;
            let filter = if path.is_dir() { "subpath" } else { "literal" };
            profile.push(&format!("(deny file-write* ({filter} {parameter}))"));
            profile.protect_ancestors(&path)?;
        }
    }

    profile.protect_directory(&workspace)?;
    profile.protect_directory(&private_temp)?;
    if let Some(git_dir) = &git_dir {
        profile.protect_directory(git_dir)?;
    }

    let mut args = vec![OsString::from("-p"), profile.source.into()];
    args.extend(profile.parameters);
    args.push("--".into());
    args.push(plan.executable.as_os_str().to_owned());
    args.extend(plan.args.iter().cloned());
    Ok(SeatbeltCommand { args, run_marker })
}

pub(super) fn random_mach_name() -> Result<String, SandboxError> {
    use std::fmt::Write;

    let mut bytes = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| std::io::Read::read_exact(&mut source, &mut bytes))
        .map_err(|error| {
            SandboxError::SandboxUnavailable(format!("cannot generate a run marker: {error}"))
        })?;
    let mut name = String::from("dev.kraai.run.");
    for byte in bytes {
        let _ = write!(name, "{byte:02x}");
    }
    Ok(name)
}

struct Profile {
    source: String,
    parameters: Vec<OsString>,
    protected_ancestors: BTreeSet<PathBuf>,
}

impl Profile {
    fn new() -> Self {
        Self {
            source: include_str!("base.sbpl").into(),
            parameters: Vec::new(),
            protected_ancestors: BTreeSet::new(),
        }
    }

    fn push(&mut self, rule: &str) {
        self.source.push_str(rule);
        self.source.push('\n');
    }

    fn parameter(&mut self, path: &Path) -> Result<String, SandboxError> {
        let value = path
            .to_str()
            .filter(|value| !value.contains('\0'))
            .ok_or_else(|| unavailable(path, "Seatbelt paths must be UTF-8 without NUL bytes"))?;
        let name = format!("PATH_{}", self.parameters.len());
        Ok(self.named_parameter(&name, value))
    }

    fn named_parameter(&mut self, name: &str, value: &str) -> String {
        self.parameters.push(format!("-D{name}={value}").into());
        format!("(param \"{name}\")")
    }

    fn path_filter(&mut self, path: &Path, directory: bool) -> Result<String, SandboxError> {
        let parameter = self.parameter(path)?;
        self.push(&format!(
            "(allow file-read-metadata (path-ancestors {parameter}))"
        ));
        Ok(if directory {
            format!("(require-any (literal {parameter}) (subpath {parameter}))")
        } else {
            format!("(literal {parameter})")
        })
    }

    fn protect_ancestors(&mut self, path: &Path) -> Result<(), SandboxError> {
        for ancestor in path.ancestors().skip(1) {
            self.protect_directory(ancestor)?;
        }
        Ok(())
    }

    fn protect_directory(&mut self, path: &Path) -> Result<(), SandboxError> {
        if self.protected_ancestors.insert(path.to_path_buf()) {
            let parameter = self.parameter(path)?;
            self.push(&format!(
                "(deny file-write-unlink (require-all (literal {parameter}) (vnode-type DIRECTORY)))"
            ));
        }
        Ok(())
    }
}

fn resolve(path: &Path) -> Result<PathBuf, SandboxError> {
    path.canonicalize()
        .map_err(|error| unavailable(path, &error.to_string()))
}

fn unavailable(path: &Path, reason: &str) -> SandboxError {
    SandboxError::SandboxUnavailable(format!(
        "cannot build Seatbelt policy for '{}': {reason}",
        path.display()
    ))
}
