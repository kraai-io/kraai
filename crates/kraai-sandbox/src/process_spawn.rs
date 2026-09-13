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

#[cfg(all(test, windows))]
#[expect(
    unsafe_code,
    reason = "test inheritance with a Windows event shared only if a handle leaks"
)]
mod tests {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};

    #[test]
    fn inheritance_probe() -> Result<(), Box<dyn std::error::Error>> {
        if let Ok(handle) = std::env::var("KRAAI_INHERITANCE_PROBE") {
            let handle = handle.parse::<usize>()? as *mut std::ffi::c_void;
            unsafe { SetEvent(handle) };
        }
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "regression tests assert observable behavior"
    )]
    fn host_spawn_waits_until_private_handle_inheritance_is_restored()
    -> Result<(), Box<dyn std::error::Error>> {
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let event = unsafe { OwnedHandle::from_raw_handle(event) };
        let handle = event.as_raw_handle() as usize;
        let executable = std::env::current_exe()?;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread = with_lock(|| -> Result<_, Box<dyn std::error::Error>> {
            assert_ne!(
                unsafe {
                    SetHandleInformation(
                        event.as_raw_handle(),
                        HANDLE_FLAG_INHERIT,
                        HANDLE_FLAG_INHERIT,
                    )
                },
                0
            );
            let thread = std::thread::spawn(move || -> io::Result<std::process::ExitStatus> {
                let mut command = Command::new(executable);
                command
                    .args(["--exact", "process_spawn::tests::inheritance_probe"])
                    .env("KRAAI_INHERITANCE_PROBE", handle.to_string());
                let _ = ready_tx.send(());
                let mut child = spawn_command(&mut command)?;
                let _ = done_tx.send(());
                child.wait()
            });
            ready_rx.recv()?;
            assert!(matches!(
                done_rx.recv_timeout(std::time::Duration::from_millis(50)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));
            assert_ne!(
                unsafe { SetHandleInformation(event.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) },
                0
            );
            Ok(thread)
        })?;
        let status = thread
            .join()
            .map_err(|_panic| io::Error::other("spawn test thread panicked"))??;
        assert!(status.success());
        assert_eq!(
            unsafe { WaitForSingleObject(event.as_raw_handle(), 0) },
            WAIT_TIMEOUT
        );
        Ok(())
    }
}
