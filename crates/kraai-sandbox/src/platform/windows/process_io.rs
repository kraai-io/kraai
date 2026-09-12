use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows_sys::Win32::Foundation::{
    GetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Pipes::CreatePipe;

pub(super) struct Pipes {
    pub(super) stdin: OwnedHandle,
    pub(super) stdout_read: OwnedHandle,
    pub(super) stdout_write: OwnedHandle,
    pub(super) stderr_read: OwnedHandle,
    pub(super) stderr_write: OwnedHandle,
}

impl Pipes {
    #[expect(
        unsafe_code,
        reason = "child stdin requires an inheritable handle to the Windows null device"
    )]
    pub(super) fn new() -> io::Result<Self> {
        let attributes = inheritable();
        let stdin = unsafe {
            CreateFileW(
                windows_sys::core::w!("NUL"),
                FILE_GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &attributes,
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if stdin == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let stdin = unsafe { OwnedHandle::from_raw_handle(stdin) };
        let (stdout_read, stdout_write) = pipe()?;
        let (stderr_read, stderr_write) = pipe()?;
        Ok(Self {
            stdin,
            stdout_read,
            stdout_write,
            stderr_read,
            stderr_write,
        })
    }
}

fn inheritable() -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    }
}

#[expect(unsafe_code, reason = "only child pipe endpoints may be inherited")]
fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    if unsafe { CreatePipe(&mut read, &mut write, &inheritable(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let read = unsafe { OwnedHandle::from_raw_handle(read) };
    let write = unsafe { OwnedHandle::from_raw_handle(write) };
    if unsafe { SetHandleInformation(read.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((read, write))
}

pub(super) struct InheritedHandles<'a> {
    handles: Vec<(&'a OwnedHandle, u32)>,
}

impl<'a> InheritedHandles<'a> {
    #[expect(
        unsafe_code,
        reason = "private IPC handles are temporarily inheritable only during native process creation"
    )]
    pub(super) fn new(handles: &'a [OwnedHandle]) -> io::Result<Self> {
        let mut inherited = Self {
            handles: Vec::new(),
        };
        for handle in handles {
            let mut flags = 0;
            if unsafe { GetHandleInformation(handle.as_raw_handle(), &mut flags) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if unsafe {
                SetHandleInformation(
                    handle.as_raw_handle(),
                    HANDLE_FLAG_INHERIT,
                    HANDLE_FLAG_INHERIT,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            inherited.handles.push((handle, flags));
        }
        Ok(inherited)
    }
}

impl Drop for InheritedHandles<'_> {
    #[expect(
        unsafe_code,
        reason = "restore inherited handle flags on every success and error path"
    )]
    fn drop(&mut self) {
        for (handle, flags) in &self.handles {
            unsafe {
                SetHandleInformation(
                    handle.as_raw_handle(),
                    HANDLE_FLAG_INHERIT,
                    flags & HANDLE_FLAG_INHERIT,
                )
            };
        }
    }
}
