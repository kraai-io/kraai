#![expect(
    unsafe_code,
    reason = "Windows named mutexes serialize sandbox filesystem mutations"
)]

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;

use windows_sys::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

use crate::SandboxError;

pub(super) struct Lock(OwnedHandle);

impl Lock {
    pub(super) fn acquire() -> Result<Self, SandboxError> {
        let name = "Global\\KraaiSandboxAclMutation"
            .encode_utf16()
            .chain([0])
            .collect::<Vec<_>>();
        let handle = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(super::unavailable("create sandbox mutation mutex"));
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let status = unsafe { WaitForSingleObject(handle.as_raw_handle(), 300_000) };
        if status != WAIT_OBJECT_0 && status != WAIT_ABANDONED {
            return Err(SandboxError::SandboxUnavailable(format!(
                "ACL mutation mutex wait failed: {status:#x}"
            )));
        }
        Ok(Self(handle))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { ReleaseMutex(self.0.as_raw_handle()) };
    }
}
