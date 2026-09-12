#![expect(
    unsafe_code,
    reason = "Windows owns SID allocations returned by its security APIs"
)]

use std::ptr;

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    CopySid, DeriveCapabilitySidsFromName, FreeSid, GetLengthSid, PSID,
};

use crate::SandboxError;

#[derive(Debug)]
pub(super) struct Sid(Vec<u32>);

impl Sid {
    unsafe fn copy(sid: PSID) -> Result<Self, SandboxError> {
        let length = unsafe { GetLengthSid(sid) };
        let mut data = vec![0_u32; (length as usize).div_ceil(size_of::<u32>())];
        if unsafe { CopySid(length, data.as_mut_ptr().cast(), sid) } == 0 {
            return Err(super::unavailable("copy security identifier"));
        }
        Ok(Self(data))
    }

    pub(super) fn as_ptr(&self) -> PSID {
        self.0.as_ptr().cast_mut().cast()
    }

    pub(super) fn bytes(&self) -> Vec<u8> {
        let length = unsafe { GetLengthSid(self.as_ptr()) } as usize;
        unsafe { std::slice::from_raw_parts(self.as_ptr().cast::<u8>(), length) }.to_vec()
    }
}

#[derive(Debug)]
pub(super) struct Identity {
    name: Vec<u16>,
    pub(super) sid: Sid,
}

impl Identity {
    pub(super) fn create() -> Result<Self, SandboxError> {
        let name = format!("kraai.{:032x}", rand::random::<u128>())
            .encode_utf16()
            .chain([0])
            .collect::<Vec<_>>();
        let mut sid = ptr::null_mut();
        let result = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                ptr::null(),
                0,
                &mut sid,
            )
        };
        if result < 0 {
            return Err(SandboxError::SandboxUnavailable(format!(
                "unable to create AppContainer profile: HRESULT {result:#x}"
            )));
        }
        let copied = unsafe { Sid::copy(sid) };
        unsafe { FreeSid(sid) };
        match copied {
            Ok(sid) => Ok(Self { name, sid }),
            Err(error) => {
                unsafe { DeleteAppContainerProfile(name.as_ptr()) };
                Err(error)
            }
        }
    }
}

impl Drop for Identity {
    fn drop(&mut self) {
        unsafe { DeleteAppContainerProfile(self.name.as_ptr()) };
    }
}

pub(super) fn capability(name: &str) -> Result<Vec<Sid>, SandboxError> {
    let name = name.encode_utf16().chain([0]).collect::<Vec<_>>();
    let mut groups = ptr::null_mut();
    let mut group_count = 0;
    let mut capabilities = ptr::null_mut();
    let mut capability_count = 0;
    let result = unsafe {
        DeriveCapabilitySidsFromName(
            name.as_ptr(),
            &mut groups,
            &mut group_count,
            &mut capabilities,
            &mut capability_count,
        )
    };
    if result == 0 {
        return Err(super::unavailable("derive AppContainer capability"));
    }
    let copied = if capability_count == 0 || capabilities.is_null() {
        Err(SandboxError::SandboxUnavailable(String::from(
            "Windows returned no capability SID",
        )))
    } else {
        unsafe { std::slice::from_raw_parts(capabilities, capability_count as usize) }
            .iter()
            .map(|sid| unsafe { Sid::copy(*sid) })
            .collect()
    };
    unsafe {
        free_sid_array(groups, group_count);
        free_sid_array(capabilities, capability_count);
    }
    copied
}

unsafe fn free_sid_array(array: *mut PSID, count: u32) {
    if !array.is_null() {
        for sid in unsafe { std::slice::from_raw_parts(array, count as usize) } {
            unsafe { LocalFree(*sid) };
        }
        unsafe { LocalFree(array.cast()) };
    }
}
