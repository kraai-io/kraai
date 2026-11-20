#![expect(
    unsafe_code,
    reason = "Windows owns SID allocations returned by its security APIs"
)]

use std::fmt::Write as _;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;

use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, GetLastError,
    LocalFree,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    CopySid, DeriveCapabilitySidsFromName, FreeSid, GetLengthSid, PSID,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE,
};

use super::acl::Access;
use crate::SandboxError;

const LAUNCH_PREFIX: &str = "kraai.l.";
const RETIRED_PREFIXES: &[&str] = &["kraai.w."];
const MAPPINGS: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppContainer\Mappings";

#[derive(Debug, Clone)]
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
}

/// A launch's own AppContainer. Its mutex stays open while the profile is in use, so a sweep
/// can tell a crashed launch's profile from a running one.
#[derive(Debug)]
pub(super) struct Identity {
    name: Vec<u16>,
    pub(super) sid: Sid,
    alive: Option<OwnedHandle>,
}

impl Identity {
    pub(super) fn create() -> Result<Self, SandboxError> {
        let moniker = format!("{LAUNCH_PREFIX}{:032x}", rand::random::<u128>());
        let alive = create_liveness_mutex(&moniker)?;
        let _lock = super::mutation_lock::Lock::acquire()?;
        let name = wide(&moniker);
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
            Ok(sid) => Ok(Self {
                name,
                sid,
                alive: Some(alive),
            }),
            Err(error) => {
                unsafe { DeleteAppContainerProfile(name.as_ptr()) };
                Err(error)
            }
        }
    }

    pub(super) fn cleanup(&mut self) -> Result<(), SandboxError> {
        self.cleanup_with(|name| {
            let _lock = super::mutation_lock::Lock::acquire()?;
            delete_profile(name)
        })
    }

    fn cleanup_with(
        &mut self,
        delete: impl FnOnce(&[u16]) -> Result<(), SandboxError>,
    ) -> Result<(), SandboxError> {
        if !self.name.is_empty() {
            delete(&self.name)?;
            self.name.clear();
        }
        self.alive = None;
        Ok(())
    }
}

impl Drop for Identity {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn create_liveness_mutex(moniker: &str) -> Result<OwnedHandle, SandboxError> {
    let name = liveness_mutex_name(moniker);
    let handle = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(super::unavailable("create AppContainer liveness mutex"));
    }
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    if existed {
        return Err(SandboxError::SandboxUnavailable(format!(
            "AppContainer liveness mutex for '{moniker}' already exists"
        )));
    }
    Ok(handle)
}

fn liveness_mutex_name(moniker: &str) -> Vec<u16> {
    wide(&format!("Global\\{moniker}"))
}

fn is_live(moniker: &str) -> bool {
    let name = liveness_mutex_name(moniker);
    let handle = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    if handle.is_null() {
        return unsafe { GetLastError() } != ERROR_FILE_NOT_FOUND;
    }
    unsafe { CloseHandle(handle) };
    true
}

/// Deletes kraai AppContainer profiles that no running launch holds, once per process.
pub(super) fn sweep_stale_profiles_once() {
    static SWEPT: std::sync::Once = std::sync::Once::new();
    SWEPT.call_once(|| {
        let _ = sweep_stale_profiles();
    });
}

fn sweep_stale_profiles() -> Result<(), SandboxError> {
    let _lock = super::mutation_lock::Lock::acquire()?;
    let mut failure = None;
    for moniker in monikers()? {
        let stale = if moniker.starts_with(LAUNCH_PREFIX) {
            !is_live(&moniker)
        } else {
            RETIRED_PREFIXES
                .iter()
                .any(|prefix| moniker.starts_with(prefix))
        };
        if stale && let Err(error) = delete_profile(&wide(&moniker)) {
            failure.get_or_insert(error);
        }
    }
    failure.map_or(Ok(()), Err)
}

fn delete_profile(name: &[u16]) -> Result<(), SandboxError> {
    let result = unsafe { DeleteAppContainerProfile(name.as_ptr()) };
    if result < 0 {
        return Err(SandboxError::SandboxUnavailable(format!(
            "unable to delete AppContainer profile '{}': HRESULT {result:#x}",
            String::from_utf16_lossy(name.trim_suffix(&[0]))
        )));
    }
    Ok(())
}

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

/// The monikers of the current user's AppContainer profiles.
fn monikers() -> Result<Vec<String>, SandboxError> {
    let path = wide(MAPPINGS);
    let mut key = ptr::null_mut();
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(Vec::new());
    }
    if status != 0 {
        return Err(registry_error("open AppContainer mappings", status));
    }
    let key = RegistryKey(key);
    let value = wide("Moniker");
    let mut monikers = Vec::new();
    for index in 0.. {
        let mut subkey = [0_u16; 256];
        let mut length = subkey.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key.0,
                index,
                subkey.as_mut_ptr(),
                &mut length,
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != 0 {
            return Err(registry_error("enumerate AppContainer mappings", status));
        }
        let mut moniker = [0_u16; 256];
        let mut size = size_of_val(&moniker) as u32;
        let status = unsafe {
            RegGetValueW(
                key.0,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                moniker.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != 0 {
            continue;
        }
        let moniker = moniker
            .get(..size as usize / size_of::<u16>())
            .unwrap_or_default();
        monikers.push(String::from_utf16_lossy(moniker.trim_suffix(&[0])));
    }
    Ok(monikers)
}

fn registry_error(operation: &str, status: u32) -> SandboxError {
    SandboxError::SandboxUnavailable(format!(
        "unable to {operation}: {}",
        std::io::Error::from_raw_os_error(status as i32)
    ))
}

/// The capability that unlocks the persistent grant of `access` on `path`. Only launches whose
/// plan grants `path` carry it, so its ACEs give nothing to other launches.
pub(super) fn path_capability(
    kind: &str,
    path: &Path,
    access: Access,
) -> Result<Vec<Sid>, SandboxError> {
    let mut hasher = Sha256::new();
    for unit in path.as_os_str().encode_wide() {
        hasher.update(unit.to_le_bytes());
    }
    let digest = hasher.finalize();
    let mut name = format!("kraai.{kind}.");
    for byte in digest.iter().take(16) {
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(match access {
        Access::Read => ".r",
        Access::Write => ".w",
    });
    capability(&name)
}

pub(super) fn capability(name: &str) -> Result<Vec<Sid>, SandboxError> {
    let name = wide(name);
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

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

#[cfg(test)]
mod tests;
