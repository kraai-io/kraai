use std::fs::File;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;

use windows_sys::Wdk::Storage::FileSystem::NtSetSecurityObject;
use windows_sys::Win32::Foundation::{
    LocalFree, RtlNtStatusToDosError, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{
    DENY_ACCESS, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetSecurityInfo, SE_FILE_OBJECT,
    SetEntriesInAclW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, DeleteAce, EqualSid, GetAce,
    InitializeSecurityDescriptor, IsValidAcl, IsValidSid, SECURITY_DESCRIPTOR,
    SetSecurityDescriptorDacl,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, WRITE_DAC,
    WRITE_OWNER,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

use super::Access;
use windows_sys::Win32::Security::{
    GetSecurityDescriptorControl, SE_DACL_AUTO_INHERITED, SE_DACL_PROTECTED,
    SetSecurityDescriptorControl,
};

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
            return Err(error("create ACL mutation mutex"));
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let status = unsafe { WaitForSingleObject(handle.as_raw_handle(), 15000) };
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

struct Allocation(*mut std::ffi::c_void);
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

pub(super) fn update(file: &File, sid: &[u32], access: Option<Access>) -> Result<(), SandboxError> {
    let mut dacl = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    let result = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if result != 0 {
        return Err(code_error("read DACL", result));
    }
    let _descriptor = Allocation(descriptor);
    if dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
        return Err(SandboxError::SandboxUnavailable(String::from(
            "sandbox roots must have a valid explicit DACL",
        )));
    }
    let mut base = without_sid(dacl, sid)?;
    let mut edited = ptr::null_mut();
    let mut copied;
    let _edited;
    let dacl = if let Some(access) = access {
        let (mode, mask) = match access {
            Access::Read => (GRANT_ACCESS, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE),
            Access::Write => (
                GRANT_ACCESS,
                FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE,
            ),
            Access::DenyWrite => (
                DENY_ACCESS,
                FILE_WRITE_DATA
                    | FILE_APPEND_DATA
                    | FILE_WRITE_EA
                    | FILE_WRITE_ATTRIBUTES
                    | FILE_DELETE_CHILD
                    | DELETE
                    | WRITE_DAC
                    | WRITE_OWNER,
            ),
        };
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: mask,
            grfAccessMode: mode,
            grfInheritance: 3,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: sid.as_ptr().cast_mut().cast(),
            },
        };
        let mut entries = vec![entry];
        if matches!(access, Access::DenyWrite) {
            let mut read = entry;
            read.grfAccessMode = GRANT_ACCESS;
            read.grfAccessPermissions = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
            entries.push(read);
        }
        let result = unsafe {
            SetEntriesInAclW(
                entries.len() as u32,
                entries.as_ptr(),
                base.as_mut_ptr().cast(),
                &mut edited,
            )
        };
        if result != 0 {
            return Err(code_error("add sandbox DACL entry", result));
        }
        _edited = Allocation(edited.cast());
        if edited.is_null() || unsafe { IsValidAcl(edited) } == 0 {
            return Err(error("validate edited DACL"));
        }
        edited
    } else {
        copied = without_sid(dacl, sid)?;
        let copy = copied.as_mut_ptr().cast::<ACL>();
        let changed = unsafe { (*copy).AceCount != (*dacl).AceCount };
        if !changed {
            return Ok(());
        }
        copy
    };
    let mut security = SECURITY_DESCRIPTOR::default();
    if unsafe {
        InitializeSecurityDescriptor((&mut security as *mut SECURITY_DESCRIPTOR).cast(), 1)
    } == 0
    {
        return Err(error("initialize DACL descriptor"));
    }
    if unsafe {
        SetSecurityDescriptorDacl(
            (&mut security as *mut SECURITY_DESCRIPTOR).cast(),
            1,
            dacl,
            0,
        )
    } == 0
    {
        return Err(error("set DACL descriptor"));
    }
    let mut control = 0;
    let mut revision = 0;
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(error("inspect DACL inheritance"));
    }
    let inherited = SE_DACL_PROTECTED | SE_DACL_AUTO_INHERITED;
    if unsafe {
        SetSecurityDescriptorControl(
            (&mut security as *mut SECURITY_DESCRIPTOR).cast(),
            inherited,
            control & inherited,
        )
    } == 0
    {
        return Err(error("preserve DACL inheritance"));
    }
    // Apply only to this pinned object; recursive Win32 ACL propagation would follow paths.
    let status = unsafe {
        NtSetSecurityObject(
            file.as_raw_handle(),
            DACL_SECURITY_INFORMATION,
            (&mut security as *mut SECURITY_DESCRIPTOR).cast(),
        )
    };
    if status < 0 {
        return Err(code_error("apply sandbox DACL", unsafe {
            RtlNtStatusToDosError(status)
        }));
    }
    Ok(())
}

fn without_sid(dacl: *mut ACL, sid: &[u32]) -> Result<Vec<u32>, SandboxError> {
    let size = unsafe { (*dacl).AclSize } as usize;
    let mut copied = vec![0_u32; size.div_ceil(size_of::<u32>())];
    unsafe { ptr::copy_nonoverlapping(dacl.cast::<u8>(), copied.as_mut_ptr().cast(), size) };
    let copy = copied.as_mut_ptr().cast::<ACL>();
    for index in (0..unsafe { (*copy).AceCount } as u32).rev() {
        let mut ace = ptr::null_mut();
        if unsafe { GetAce(copy, index, &mut ace) } == 0 {
            return Err(error("inspect DACL entry"));
        }
        let header = std::ptr::NonNull::new(ace.cast::<ACE_HEADER>())
            .ok_or_else(|| error("validate DACL entry"))?;
        let header = unsafe { header.as_ref() };
        if matches!(header.AceType, 0 | 1)
            && header.AceSize as usize >= size_of::<ACE_HEADER>() + size_of::<u32>() + 8
        {
            let ace_sid = unsafe { ace.cast::<u8>().add(8) }.cast();
            if unsafe { IsValidSid(ace_sid) } == 0 {
                return Err(error("validate DACL entry SID"));
            }
            if unsafe { EqualSid(ace_sid, sid.as_ptr().cast_mut().cast()) } != 0
                && unsafe { DeleteAce(copy, index) } == 0
            {
                return Err(error("remove sandbox DACL entry"));
            }
        }
    }
    Ok(copied)
}

fn error(operation: &str) -> SandboxError {
    SandboxError::SandboxUnavailable(format!(
        "unable to {operation}: {}",
        std::io::Error::last_os_error()
    ))
}

fn code_error(operation: &str, code: u32) -> SandboxError {
    SandboxError::SandboxUnavailable(format!(
        "unable to {operation}: {}",
        std::io::Error::from_raw_os_error(code as i32)
    ))
}
