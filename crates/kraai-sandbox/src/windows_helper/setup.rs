#![expect(
    unsafe_code,
    reason = "install and query a protected Windows service through SCM"
)]

use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr;
use windows_sys::Win32::Foundation::{
    ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST, HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Services::*;
use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath};

struct Service(SC_HANDLE);
impl Drop for Service {
    fn drop(&mut self) {
        unsafe { CloseServiceHandle(self.0) };
    }
}

fn manager(access: u32) -> io::Result<Service> {
    let handle = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), access) };
    if handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(Service(handle))
    }
}

fn open(manager: &Service, access: u32) -> io::Result<Service> {
    let handle = unsafe { OpenServiceW(manager.0, super::wide(super::SERVICE).as_ptr(), access) };
    if handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(Service(handle))
    }
}

fn status(service: &Service) -> io::Result<SERVICE_STATUS_PROCESS> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut length = 0;
    if unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(status)
}

fn verify_configuration(service: &Service, executable: &std::path::Path) -> io::Result<()> {
    let mut length = 0;
    unsafe { QueryServiceConfigW(service.0, ptr::null_mut(), 0, &mut length) };
    if length == 0 || length > 65536 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0_u64; (length as usize).div_ceil(8)];
    if unsafe { QueryServiceConfigW(service.0, buffer.as_mut_ptr().cast(), length, &mut length) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    let config = unsafe { &*buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    let expected = super::wide(&format!("\"{}\" service", executable.display()));
    let mut actual_length = 0;
    while unsafe { *config.lpBinaryPathName.add(actual_length) } != 0 {
        actual_length += 1;
    }
    let actual = unsafe { std::slice::from_raw_parts(config.lpBinaryPathName, actual_length + 1) };
    if config.dwServiceType != SERVICE_WIN32_OWN_PROCESS || actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "KraaiSandbox belongs to a different service installation",
        ));
    }
    Ok(())
}

pub(super) fn verify_server(pipe: HANDLE) -> io::Result<()> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = open(&manager, SERVICE_QUERY_STATUS)?;
    let state = status(&service)?;
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if state.dwCurrentState != SERVICE_RUNNING || state.dwProcessId == 0 || state.dwProcessId != pid
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "sandbox pipe is not owned by the installed service",
        ));
    }
    Ok(())
}

fn directory() -> io::Result<PathBuf> {
    let mut value = ptr::null_mut();
    let result =
        unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, ptr::null_mut(), &mut value) };
    if result < 0 {
        return Err(io::Error::other(format!(
            "cannot locate Program Files: HRESULT {result:#x}"
        )));
    }
    let mut length = 0;
    while unsafe { *value.add(length) } != 0 {
        length += 1;
    }
    let path = PathBuf::from(std::ffi::OsString::from_wide(unsafe {
        std::slice::from_raw_parts(value, length)
    }));
    unsafe { CoTaskMemFree(value.cast()) };
    Ok(path.join("KraaiSandbox"))
}

fn protected_directory(path: &std::path::Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let mut descriptor = ptr::null_mut();
    let sddl = super::wide("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;BU)");
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
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let path = path
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<_>>();
    let result = unsafe { CreateDirectoryW(path.as_ptr(), &attributes) };
    let error = (result == 0).then(io::Error::last_os_error);
    unsafe { LocalFree(descriptor) };
    error.map_or(Ok(()), Err)
}

pub(super) fn install() -> io::Result<()> {
    let manager = manager(SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE)?;
    let directory = directory()?;
    let executable = directory.join("kraai-sandbox-helper.exe");
    let source = std::env::current_exe()?;
    match open(
        &manager,
        SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG | SERVICE_START,
    ) {
        Ok(service) => {
            verify_configuration(&service, &executable)?;
            if std::fs::read(&source)? != std::fs::read(&executable)? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "uninstall the previous helper before installing this build",
                ));
            }
            return start(&service);
        }
        Err(error) if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {}
        Err(error) => return Err(error),
    }
    protected_directory(&directory)?;
    let result = (|| {
        let mut source = std::fs::File::open(source)?;
        let mut destination = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&executable)?;
        io::copy(&mut source, &mut destination)?;
        destination.sync_all()?;
        drop(destination);
        let command = super::wide(&format!("\"{}\" service", executable.display()));
        let name = super::wide(super::SERVICE);
        let handle = unsafe {
            CreateServiceW(
                manager.0,
                name.as_ptr(),
                name.as_ptr(),
                SERVICE_ALL_ACCESS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                command.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
            )
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let service = Service(handle);
        if let Err(error) = start(&service) {
            let mut state = SERVICE_STATUS::default();
            unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut state) };
            let _ = wait_for(&service, SERVICE_STOPPED);
            unsafe { DeleteService(service.0) };
            return Err(error);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&executable);
        let _ = std::fs::remove_dir(&directory);
    }
    result
}

fn start(service: &Service) -> io::Result<()> {
    if unsafe { StartServiceW(service.0, 0, ptr::null()) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_SERVICE_ALREADY_RUNNING as i32) {
            return Err(error);
        }
    }
    wait_for(service, SERVICE_RUNNING)
}

fn wait_for(service: &Service, expected: u32) -> io::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let current = status(service)?;
        if current.dwCurrentState == expected {
            return Ok(());
        }
        if expected == SERVICE_RUNNING && current.dwCurrentState == SERVICE_STOPPED {
            return Err(io::Error::other(format!(
                "sandbox service stopped during startup: {}",
                current.dwWin32ExitCode
            )));
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "sandbox service transition timed out",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

pub(super) fn uninstall() -> io::Result<()> {
    let directory = directory()?;
    let executable = directory.join("kraai-sandbox-helper.exe");
    if std::env::current_exe()?.canonicalize()? == executable.canonicalize()? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "run uninstall from the original helper executable outside Program Files",
        ));
    }
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = open(
        &manager,
        SERVICE_STOP
            | SERVICE_QUERY_STATUS
            | SERVICE_QUERY_CONFIG
            | windows_sys::Win32::Storage::FileSystem::DELETE,
    )?;
    verify_configuration(&service, &executable)?;
    if status(&service)?.dwCurrentState != SERVICE_STOPPED {
        let mut value = SERVICE_STATUS::default();
        if unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut value) } == 0 {
            return Err(io::Error::last_os_error());
        }
        wait_for(&service, SERVICE_STOPPED)?;
    }
    super::journal::Journal::open()?.recover()?;
    if unsafe { DeleteService(service.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    std::fs::remove_file(executable)?;
    std::fs::remove_dir(directory)
}
