use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kraai_types::SandboxCapabilities;
use tokio::sync::mpsc::UnboundedSender;

use crate::output::OutputEvent;

#[derive(Debug)]
pub struct LaunchPlan {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub workspace_root: PathBuf,
    pub environment: BTreeMap<OsString, OsString>,
    pub runtime_roots: Vec<PathBuf>,
    pub capabilities: SandboxCapabilities,
    pub timeout: Duration,
    pub output_events: Option<UnboundedSender<OutputEvent>>,
    pub private_temp: PrivateTempConfig,
    #[cfg(unix)]
    pub private_ipc_connect_descriptors: Vec<std::os::fd::RawFd>,
}

impl LaunchPlan {
    pub fn new(
        executable: PathBuf,
        workspace_root: PathBuf,
        capabilities: SandboxCapabilities,
        timeout: Duration,
    ) -> Self {
        Self {
            executable,
            args: Vec::new(),
            workspace_root,
            environment: BTreeMap::new(),
            runtime_roots: Vec::new(),
            capabilities,
            timeout,
            output_events: None,
            private_temp: PrivateTempConfig::default(),
            #[cfg(unix)]
            private_ipc_connect_descriptors: Vec::new(),
        }
    }

    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub fn args(&mut self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> &mut Self {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }
}

#[derive(Debug, Default)]
pub struct PrivateTempConfig {
    pub base_dir: Option<PathBuf>,
    pub(crate) reservation: Option<crate::temp_dir::PrivateTempDir>,
}

impl PrivateTempConfig {
    pub fn under(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: Some(base_dir.into()),
            reservation: None,
        }
    }

    pub fn reserve(mut self) -> Result<Self, crate::SandboxError> {
        if self.reservation.is_none() {
            self.reservation = Some(crate::temp_dir::PrivateTempDir::create(
                self.base_dir.as_deref(),
            )?);
        }
        Ok(self)
    }

    pub fn path(&self) -> Option<&Path> {
        self.reservation
            .as_ref()
            .map(crate::temp_dir::PrivateTempDir::path)
    }

    pub(crate) fn into_private_temp(
        self,
    ) -> Result<crate::temp_dir::PrivateTempDir, crate::SandboxError> {
        self.reservation.map_or_else(
            || crate::temp_dir::PrivateTempDir::create(self.base_dir.as_deref()),
            Ok,
        )
    }
}

#[derive(Debug)]
pub(crate) struct PreparedCommand {
    pub(crate) executable: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) cwd: PathBuf,
    pub(crate) environment: BTreeMap<OsString, OsString>,
    pub(crate) sandboxed: bool,
    pub(crate) output_events: Option<UnboundedSender<OutputEvent>>,
    pub(crate) private_temp: crate::temp_dir::PrivateTempDir,
    #[cfg(target_os = "linux")]
    pub(crate) seccomp_filter: Option<std::fs::File>,
}

impl PreparedCommand {
    pub(crate) fn unsandboxed(
        plan: LaunchPlan,
        private_temp: crate::temp_dir::PrivateTempDir,
    ) -> Self {
        let LaunchPlan {
            executable,
            args,
            workspace_root,
            mut environment,
            output_events,
            ..
        } = plan;
        private_temp.apply_environment(&mut environment);
        Self {
            executable,
            args,
            cwd: workspace_root,
            environment,
            sandboxed: false,
            output_events,
            private_temp,
            #[cfg(target_os = "linux")]
            seccomp_filter: None,
        }
    }
}

pub(crate) fn path_is_absolute(path: &Path) -> bool {
    path.is_absolute()
}
