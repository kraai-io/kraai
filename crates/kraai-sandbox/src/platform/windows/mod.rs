mod acl;
mod identity;
pub(crate) mod private_temp;
pub(crate) mod process;

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf, Prefix};

use kraai_types::SandboxCapability;
use windows_sys::Win32::Security::{SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES};

use crate::SandboxError;
use crate::config::{LaunchPlan, PreparedCommand};
use crate::temp_dir::PrivateTempDir;

use acl::{Access, Grants};
use identity::{Identity, Sid};

#[derive(Debug)]
pub(crate) struct Sandbox {
    grants: Grants,
    identity: Identity,
    capabilities: Vec<Sid>,
}

impl Sandbox {
    pub(crate) fn cleanup(&mut self) -> Result<(), SandboxError> {
        self.grants.cleanup()
    }

    pub(crate) fn security_capabilities(&self) -> (SECURITY_CAPABILITIES, Vec<SID_AND_ATTRIBUTES>) {
        let capabilities = self
            .capabilities
            .iter()
            .map(|sid| SID_AND_ATTRIBUTES {
                Sid: sid.as_ptr(),
                Attributes: 4,
            })
            .collect::<Vec<_>>();
        (
            SECURITY_CAPABILITIES {
                AppContainerSid: self.identity.sid.as_ptr(),
                Capabilities: std::ptr::null_mut(),
                CapabilityCount: capabilities.len() as u32,
                Reserved: 0,
            },
            capabilities,
        )
    }
}

pub(crate) fn prepare(
    mut plan: LaunchPlan,
    private_temp: PrivateTempDir,
) -> Result<PreparedCommand, SandboxError> {
    if plan.capabilities.contains(SandboxCapability::HostRead) {
        return Err(SandboxError::SandboxUnavailable(String::from(
            "Windows AppContainer does not support host-read or host-write; configure runtime roots and workspace capabilities instead",
        )));
    }
    plan.workspace_root = local_path(&plan.workspace_root)?;
    plan.executable = local_path(&plan.executable)?;
    let runtime_roots = plan
        .runtime_roots
        .iter()
        .map(|path| local_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    let temp_path = local_path(private_temp.path())?;
    let system_root = system_root()?;
    let identity = Identity::create()?;
    let mut capabilities = identity::capability("registryRead")?;
    if plan.capabilities.contains(SandboxCapability::Network) {
        for name in [
            "internetClient",
            "internetClientServer",
            "privateNetworkClientServer",
            "lpacCryptoServices",
        ] {
            capabilities.extend(identity::capability(name)?);
        }
    }
    let mut grants = Grants::new(identity.sid.bytes());
    let writable = plan
        .capabilities
        .contains(SandboxCapability::WorkspaceWrite);
    for root in runtime_roots {
        if !root.starts_with(&plan.workspace_root) && !root.starts_with(&system_root) {
            grants.grant(&root, Access::Read)?;
        }
    }
    grants.grant(
        &plan.workspace_root,
        if writable {
            Access::Write
        } else {
            Access::Read
        },
    )?;
    if writable && !plan.capabilities.contains(SandboxCapability::MetadataWrite) {
        for path in super::metadata::protected_paths(&plan.workspace_root)? {
            match path.try_exists() {
                Ok(true) => grants.grant(&path, Access::DenyWrite)?,
                Ok(false) => {}
                Err(error) => {
                    return Err(SandboxError::SandboxUnavailable(format!(
                        "unable to inspect metadata '{}': {error}",
                        path.display()
                    )));
                }
            }
        }
    }
    grants.grant(&temp_path, Access::Write)?;
    plan.environment
        .retain(|name, _| !name.to_string_lossy().eq_ignore_ascii_case("SystemRoot"));
    plan.environment
        .insert(OsString::from("SystemRoot"), system_root.into_os_string());
    private_temp.apply_environment(&mut plan.environment);
    Ok(PreparedCommand {
        executable: plan.executable,
        args: plan.args,
        cwd: plan.workspace_root,
        environment: plan.environment,
        sandboxed: true,
        output_events: plan.output_events,
        private_temp: Some(private_temp),
        windows_sandbox: Some(Sandbox {
            grants: grants,
            identity,
            capabilities,
        }),
        private_ipc_handles: plan.private_ipc_handles,
    })
}

fn local_path(path: &Path) -> Result<PathBuf, SandboxError> {
    let canonical = path.canonicalize().map_err(|error| {
        SandboxError::SandboxUnavailable(format!("unable to resolve '{}': {error}", path.display()))
    })?;
    if !matches!(canonical.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
    {
        return Err(SandboxError::SandboxUnavailable(format!(
            "Windows sandbox roots must be local disk paths: '{}'",
            path.display()
        )));
    }
    if canonical.components().any(|component| {
        matches!(component, Component::Normal(name) if name.to_string_lossy().contains(':'))
    }) {
        return Err(SandboxError::SandboxUnavailable(format!(
            "alternate data streams cannot be sandbox roots: '{}'", path.display()
        )));
    }
    Ok(canonical)
}

#[expect(
    unsafe_code,
    reason = "the Windows directory must come from the OS rather than an untrusted environment variable"
)]
fn system_root() -> Result<PathBuf, SandboxError> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;

    let mut buffer = vec![0_u16; 32768];
    let length = unsafe { GetWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(unavailable("locate Windows directory"));
    }
    buffer.truncate(length);
    local_path(&PathBuf::from(OsString::from_wide(&buffer)))
}

fn unavailable(operation: &str) -> SandboxError {
    SandboxError::SandboxUnavailable(format!(
        "unable to {operation}: {}",
        std::io::Error::last_os_error()
    ))
}
