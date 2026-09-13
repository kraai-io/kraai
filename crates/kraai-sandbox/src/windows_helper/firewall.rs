#![expect(
    unsafe_code,
    reason = "manage loopback exemptions through the Windows network isolation API"
)]

use std::io;
use std::ptr;
use windows_sys::Win32::NetworkManagement::WindowsFirewall::{
    NetworkIsolationGetAppContainerConfig, NetworkIsolationSetAppContainerConfig,
};
use windows_sys::Win32::Security::Isolation::DeriveAppContainerSidFromAppContainerName;
use windows_sys::Win32::Security::{EqualSid, FreeSid, SID_AND_ATTRIBUTES};
use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapFree};

pub(super) fn profile_sid(profile: &str) -> io::Result<String> {
    let mut sid = ptr::null_mut();
    let status = unsafe {
        DeriveAppContainerSidFromAppContainerName(super::wide(profile).as_ptr(), &mut sid)
    };
    if status < 0 {
        return Err(io::Error::other(format!(
            "cannot derive sandbox SID: HRESULT {status:#x}"
        )));
    }
    let result = super::identity::sid_string(sid);
    unsafe { FreeSid(sid) };
    result
}

pub(super) fn contains(sid: &str) -> io::Result<bool> {
    let sid = super::identity::Sid::parse(sid)?;
    let entries = Config::get()?;
    Ok(entries
        .entries()
        .iter()
        .any(|entry| unsafe { EqualSid(entry.Sid, sid.as_ptr()) } != 0))
}

pub(super) fn update(sid: &str, enabled: bool) -> io::Result<()> {
    let sid = super::identity::Sid::parse(sid)?;
    let entries = Config::get()?;
    let mut retained = entries
        .entries()
        .iter()
        .filter(|entry| unsafe { EqualSid(entry.Sid, sid.as_ptr()) } == 0)
        .copied()
        .collect::<Vec<_>>();
    if enabled {
        retained.push(SID_AND_ATTRIBUTES {
            Sid: sid.as_ptr(),
            Attributes: 0,
        });
    }
    super::check(unsafe {
        NetworkIsolationSetAppContainerConfig(retained.len() as u32, retained.as_ptr())
    })
}

struct Config {
    count: u32,
    entries: *mut SID_AND_ATTRIBUTES,
}
impl Config {
    fn get() -> io::Result<Self> {
        let mut value = Self {
            count: 0,
            entries: ptr::null_mut(),
        };
        super::check(unsafe {
            NetworkIsolationGetAppContainerConfig(&mut value.count, &mut value.entries)
        })?;
        Ok(value)
    }

    fn entries(&self) -> &[SID_AND_ATTRIBUTES] {
        if self.entries.is_null() || self.count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.entries, self.count as usize) }
        }
    }
}
impl Drop for Config {
    fn drop(&mut self) {
        unsafe {
            let heap = GetProcessHeap();
            for entry in self.entries() {
                HeapFree(heap, 0, entry.Sid);
            }
            HeapFree(heap, 0, self.entries.cast());
        }
    }
}
