use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use crate::SandboxError;

pub(super) struct ProcessGroup {
    leader: Option<Pid>,
    #[cfg(target_os = "macos")]
    run_marker: Option<String>,
}

impl ProcessGroup {
    pub(super) fn new(pid: Option<u32>) -> Result<Self, SandboxError> {
        let pid = pid.ok_or_else(|| SandboxError::Wait("spawned process has no PID".into()))?;
        let pid = i32::try_from(pid).map_err(|error| SandboxError::Wait(error.to_string()))?;
        Ok(Self {
            leader: Some(Pid::from_raw(pid)),
            #[cfg(target_os = "macos")]
            run_marker: None,
        })
    }

    #[cfg(target_os = "macos")]
    pub(super) fn track_run(&mut self, run_marker: Option<String>) {
        self.run_marker = run_marker;
    }

    pub(super) fn kill(&mut self) -> Result<(), SandboxError> {
        if let Some(pid) = self.leader {
            if let Err(error) = kill_group(pid)
                && error != nix::errno::Errno::ESRCH
            {
                return Err(SandboxError::Wait(format!(
                    "unable to terminate process group: {error}"
                )));
            }
            self.leader = None;
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub(super) async fn kill_detached(&mut self) -> Result<(), SandboxError> {
        match self.run_marker.take() {
            Some(run_marker) => crate::platform::macos::members::terminate(run_marker).await,
            None => Ok(()),
        }
    }
}

fn kill_group(pid: Pid) -> nix::Result<()> {
    #[cfg(target_os = "macos")]
    {
        retry_exiting_group(
            || killpg(pid, Signal::SIGKILL),
            || {
                std::thread::sleep(std::time::Duration::from_millis(10));
            },
        )
    }
    #[cfg(not(target_os = "macos"))]
    killpg(pid, Signal::SIGKILL)
}

#[cfg(any(target_os = "macos", test))]
fn retry_exiting_group(
    mut signal: impl FnMut() -> nix::Result<()>,
    mut pause: impl FnMut(),
) -> nix::Result<()> {
    // Darwin can return EPERM while an exiting group has no signalable members.
    for _ in 0..10 {
        match signal() {
            Err(nix::errno::Errno::EPERM) => pause(),
            result => return result,
        }
    }
    signal()
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = self.kill();
        #[cfg(target_os = "macos")]
        if let Some(run_marker) = self.run_marker.take() {
            let _ = crate::platform::macos::members::terminate_blocking(&run_marker);
        }
    }
}

#[cfg(test)]
mod tests;
