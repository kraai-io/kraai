#![expect(
    unsafe_code,
    reason = "persist owned exemptions in an administrator-controlled registry key"
)]

use std::io;
use std::ptr;
use windows_sys::Win32::System::Registry::*;

pub(super) struct Journal(HKEY);
impl Journal {
    pub(super) fn open() -> io::Result<Self> {
        let mut key = ptr::null_mut();
        super::check(unsafe {
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
                super::wide(r"SOFTWARE\Kraai\Sandbox\LoopbackLeases").as_ptr(),
                0,
                ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_ALL_ACCESS | KEY_WOW64_64KEY,
                ptr::null(),
                &mut key,
                ptr::null_mut(),
            )
        })?;
        Ok(Self(key))
    }

    pub(super) fn contains(&self, sid: &str) -> io::Result<bool> {
        let status = unsafe {
            RegQueryValueExW(
                self.0,
                super::wide(sid).as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status == 2 {
            Ok(false)
        } else {
            super::check(status).map(|()| true)
        }
    }

    pub(super) fn add(&self, sid: &str) -> io::Result<()> {
        let value = 1_u32;
        super::check(unsafe {
            RegSetValueExW(
                self.0,
                super::wide(sid).as_ptr(),
                0,
                REG_DWORD,
                (&value as *const u32).cast(),
                4,
            )
        })?;
        super::check(unsafe { RegFlushKey(self.0) })
    }

    pub(super) fn remove(&self, sid: &str) -> io::Result<()> {
        let status = unsafe { RegDeleteValueW(self.0, super::wide(sid).as_ptr()) };
        if status == 2 {
            Ok(())
        } else {
            super::check(status)
        }
    }

    pub(super) fn recover(&self) -> io::Result<()> {
        loop {
            let mut name = vec![0_u16; 256];
            let mut length = name.len() as u32;
            let status = unsafe {
                RegEnumValueW(
                    self.0,
                    0,
                    name.as_mut_ptr(),
                    &mut length,
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if status == 259 {
                return Ok(());
            }
            super::check(status)?;
            name.truncate(length as usize);
            let sid = String::from_utf16(&name).map_err(io::Error::other)?;
            if !sid.starts_with("S-1-15-2-") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid sandbox lease journal",
                ));
            }
            super::firewall::update(&sid, false)?;
            self.remove(&sid)?;
        }
    }
}
impl Drop for Journal {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}
