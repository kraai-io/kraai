use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use kraai_types::SandboxCapability;

use crate::SandboxError;
use crate::config::LaunchPlan;
use crate::platform::metadata::protected_paths;

pub(super) fn build_args(
    plan: &LaunchPlan,
    private_temp: &Path,
) -> Result<Vec<OsString>, SandboxError> {
    let workspace = resolve(&plan.workspace_root)?;
    let private_temp = resolve(private_temp)?;
    let mut profile = Profile::new();
    let workspace_filter = profile.path_filter(&workspace, workspace.is_dir())?;
    let temp_filter = profile.path_filter(&private_temp, true)?;

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
                "(deny file-write* (require-all {filter} (require-not {workspace_filter}) (require-not {temp_filter})))"
            ));
            profile.protect_ancestors(&root)?;
        }
    }

    if plan.capabilities.contains(SandboxCapability::Network) {
        profile.push(include_str!("network.sbpl"));
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

    if !plan.capabilities.contains(SandboxCapability::MetadataWrite) {
        for path in protected_paths(&workspace)? {
            profile.deny_writes(&path)?;
        }
    }
    profile.protect_directory(&workspace)?;
    profile.protect_directory(&private_temp)?;

    let mut args = vec![OsString::from("-p"), profile.source.into()];
    args.extend(profile.parameters);
    args.push("--".into());
    args.push(plan.executable.as_os_str().to_owned());
    args.extend(plan.args.iter().cloned());
    Ok(args)
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
        self.parameters.push(format!("-D{name}={value}").into());
        Ok(format!("(param \"{name}\")"))
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

    fn deny_writes(&mut self, path: &Path) -> Result<(), SandboxError> {
        let filter = self.path_filter(path, true)?;
        self.push(&format!("(deny file-write* {filter})"));
        self.protect_ancestors(path)
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
