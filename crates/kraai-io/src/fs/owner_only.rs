#![expect(
    unsafe_code,
    reason = "create Windows files and directories with a protected owner-only security descriptor"
)]

use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{
    GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Creates a directory that only the current user and SYSTEM can open. Entries created inside
/// it inherit the same access.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    let path = wide_path(path)?;
    let descriptor = OwnerOnlyDescriptor::new(true)?;
    let attributes = descriptor.attributes();
    // SAFETY: The path is NUL-terminated and the descriptor outlives the call.
    if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Creates a new file that only the current user and SYSTEM can open, failing if it exists.
pub(super) fn create_private_file(path: &Path) -> io::Result<File> {
    let path = wide_path(path)?;
    let descriptor = OwnerOnlyDescriptor::new(false)?;
    let attributes = descriptor.attributes();
    // SAFETY: The path is NUL-terminated and the descriptor outlives the call.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a valid handle that nothing else owns.
    Ok(unsafe { File::from_raw_handle(handle) })
}

struct OwnerOnlyDescriptor(PSECURITY_DESCRIPTOR);

impl OwnerOnlyDescriptor {
    fn new(inheritable: bool) -> io::Result<Self> {
        let flags = if inheritable { "OICI" } else { "" };
        let user = current_user_sid()?;
        let sddl = format!("D:P(A;{flags};FA;;;SY)(A;{flags};FA;;;{user})")
            .encode_utf16()
            .chain([0])
            .collect::<Vec<_>>();
        let mut descriptor = ptr::null_mut();
        // SAFETY: The SDDL string is NUL-terminated, and Windows allocates the descriptor that
        // Drop releases.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for OwnerOnlyDescriptor {
    fn drop(&mut self) {
        // SAFETY: The descriptor came from ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

fn current_user_sid() -> io::Result<String> {
    let mut token = ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo handle, and token receives a new handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken returned a handle that nothing else owns.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut buffer = [0_u64; 64];
    let mut length = 0;
    // SAFETY: The buffer is writable for the length passed and aligned for TOKEN_USER.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer) as u32,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetTokenInformation filled the buffer with a TOKEN_USER.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = ptr::null_mut();
    // SAFETY: The SID points into the buffer, which is still alive.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut length = 0;
    // SAFETY: ConvertSidToStringSidW returned a NUL-terminated string.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: The string has `length` initialized units before its terminator.
    let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    // SAFETY: Windows allocated the string with LocalAlloc.
    unsafe { LocalFree(text.cast()) };
    Ok(value)
}

pub(super) fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut path: Vec<u16> = path.as_os_str().encode_wide().collect();
    if path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    path.push(0);
    Ok(path)
}
