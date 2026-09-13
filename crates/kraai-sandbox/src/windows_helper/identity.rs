#![expect(
    unsafe_code,
    reason = "inspect authenticated Windows tokens and copy OS-owned SIDs"
)]

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;

use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
};

pub(super) struct Sid(*mut std::ffi::c_void);

impl Sid {
    pub(super) fn parse(value: &str) -> io::Result<Self> {
        let mut sid = ptr::null_mut();
        if unsafe { ConvertStringSidToSidW(super::wide(value).as_ptr(), &mut sid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(sid))
    }

    pub(super) fn as_ptr(&self) -> PSID {
        self.0
    }
}

impl Drop for Sid {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

pub(crate) fn profile_name(nonce: &[u8; 16]) -> io::Result<String> {
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    profile_for_token(token.as_raw_handle(), nonce)
}

pub(super) fn authenticate(pipe: HANDLE, nonce: &[u8; 16]) -> io::Result<String> {
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let guard = Impersonation;
    let mut token = ptr::null_mut();
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut app_container = 0_u32;
    let mut length = 0;
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenIsAppContainer,
            (&mut app_container as *mut u32).cast(),
            4,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let integrity = information(token.as_raw_handle(), TokenIntegrityLevel)?;
    let label = unsafe { &*integrity.as_ptr().cast::<TOKEN_MANDATORY_LABEL>() };
    let count = unsafe { *GetSidSubAuthorityCount(label.Label.Sid) };
    let level = if count == 0 {
        0
    } else {
        unsafe { *GetSidSubAuthority(label.Label.Sid, u32::from(count - 1)) }
    };
    if app_container != 0
        || level < 0x2000
        || unsafe { IsTokenRestricted(token.as_raw_handle()) } != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "restricted clients cannot configure the sandbox",
        ));
    }
    let name = profile_for_token(token.as_raw_handle(), nonce);
    drop(guard);
    name
}

fn profile_for_token(token: HANDLE, nonce: &[u8; 16]) -> io::Result<String> {
    let user = information(token, TokenUser)?;
    let user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
    let length = unsafe { GetLengthSid(user.User.Sid) } as usize;
    let bytes = unsafe { std::slice::from_raw_parts(user.User.Sid.cast::<u8>(), length) };
    let mut hash = Sha256::new();
    hash.update(bytes);
    hash.update(nonce);
    let mut name = String::from("kraai.");
    for byte in hash.finalize().iter().take(28) {
        use std::fmt::Write;
        write!(name, "{byte:02x}").map_err(io::Error::other)?;
    }
    Ok(name)
}

fn information(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
    let mut length = 0;
    unsafe { GetTokenInformation(token, class, ptr::null_mut(), 0, &mut length) };
    if length == 0 || length > 65536 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0_u64; (length as usize).div_ceil(8)];
    if unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

pub(super) fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut length = 0;
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    unsafe { LocalFree(text.cast()) };
    Ok(value)
}

struct Impersonation;
impl Drop for Impersonation {
    fn drop(&mut self) {
        if unsafe { RevertToSelf() } == 0 {
            std::process::abort();
        }
    }
}
