#![expect(
    unsafe_code,
    reason = "create a Windows directory with a protected owner-only security descriptor"
)]

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

pub(crate) fn create(path: &Path) -> std::io::Result<()> {
    let path = path
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<_>>();
    if path
        .iter()
        .take(path.len().saturating_sub(1))
        .any(|unit| *unit == 0)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "temporary directory path contains NUL",
        ));
    }
    let sddl = "D:P(A;;FA;;;SY)(A;;FA;;;OW)"
        .encode_utf16()
        .chain([0])
        .collect::<Vec<_>>();
    let mut descriptor = ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let result = unsafe { CreateDirectoryW(path.as_ptr(), &attributes) };
    let error = (result == 0).then(std::io::Error::last_os_error);
    unsafe { LocalFree(descriptor) };
    error.map_or(Ok(()), Err)
}
