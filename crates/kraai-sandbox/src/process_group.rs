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
            if let Err(error) = kill_group(pid)
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
    }
}

#[cfg(test)]
mod tests {
    use super::retry_exiting_group;
    use nix::errno::Errno;

    #[test]
    fn exiting_group_is_retried_until_it_disappears() {
        let mut calls = 0;
        let result = retry_exiting_group(
            || {
                calls += 1;
                Err(if calls == 1 {
                    Errno::EPERM
                } else {
                    Errno::ESRCH
                })
            },
            || {},
        );
        assert_eq!(result, Err(Errno::ESRCH));
        assert_eq!(calls, 2);
    }

    #[test]
    fn persistent_permission_errors_remain_errors() {
        let mut pauses = 0;
        assert_eq!(
            retry_exiting_group(|| Err(Errno::EPERM), || pauses += 1),
            Err(Errno::EPERM)
        );
        assert_eq!(pauses, 10);
    }
}
