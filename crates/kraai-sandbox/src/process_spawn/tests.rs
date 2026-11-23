use super::*;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};

#[test]
fn inheritance_probe() {
    if let Ok(handle) = std::env::var("KRAAI_INHERITANCE_PROBE") {
        let handle = handle.parse::<usize>().unwrap() as *mut std::ffi::c_void;
        unsafe { SetEvent(handle) };
    }
}

#[test]
fn host_spawn_waits_until_private_handle_inheritance_is_restored() {
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        panic!("{}", io::Error::last_os_error());
    }
    let event = unsafe { OwnedHandle::from_raw_handle(event) };
    let handle = event.as_raw_handle() as usize;
    let executable = std::env::current_exe().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let thread = with_lock(|| {
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
        ready_rx.recv().unwrap();
        assert!(matches!(
            done_rx.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        assert_ne!(
            unsafe { SetHandleInformation(event.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) },
            0
        );
        thread
    });
    let status = thread.join().expect("spawn test thread panicked").unwrap();
    assert!(status.success());
    assert_eq!(
        unsafe { WaitForSingleObject(event.as_raw_handle(), 0) },
        WAIT_TIMEOUT
    );
}
