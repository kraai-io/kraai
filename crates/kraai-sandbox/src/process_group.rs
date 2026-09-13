use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use crate::SandboxError;

pub(super) struct ProcessGroup(Option<Pid>);

impl ProcessGroup {
    pub(super) fn new(pid: Option<u32>) -> Result<Self, SandboxError> {
        let pid = pid.ok_or_else(|| SandboxError::Wait("spawned process has no PID".into()))?;
        let pid = i32::try_from(pid).map_err(|error| SandboxError::Wait(error.to_string()))?;
        Ok(Self(Some(Pid::from_raw(pid))))
    }

    pub(super) fn kill(&mut self) -> Result<(), SandboxError> {
        if let Some(pid) = self.0 {
            if let Err(error) = killpg(pid, Signal::SIGKILL)
                && error != nix::errno::Errno::ESRCH
            {
                return Err(SandboxError::Wait(format!(
                    "unable to terminate process group: {error}"
                )));
            }
            self.0 = None;
        }
        Ok(())
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}
