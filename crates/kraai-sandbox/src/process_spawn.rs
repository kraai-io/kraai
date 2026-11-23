use std::io;
use std::process::{Child, Command};

#[cfg(windows)]
pub(crate) fn with_lock<T>(spawn: impl FnOnce() -> T) -> T {
    static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = SPAWN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let result = spawn();
    drop(guard);
    result
}

/// Use this for host process launches that can overlap Windows sandbox execution.
/// It excludes launches while private sandbox handles are temporarily inheritable.
pub fn spawn_command(command: &mut Command) -> io::Result<Child> {
    #[cfg(windows)]
    {
        with_lock(|| command.spawn())
    }
    #[cfg(not(windows))]
    {
        command.spawn()
    }
}

pub fn spawn_tokio_command(
    command: &mut tokio::process::Command,
) -> io::Result<tokio::process::Child> {
    #[cfg(windows)]
    {
        with_lock(|| command.spawn())
    }
    #[cfg(not(windows))]
    {
        command.spawn()
    }
}

#[cfg(test)]
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "test inheritance with a Windows event shared only if a handle leaks"
)]
mod tests;
