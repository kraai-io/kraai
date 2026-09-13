#![expect(
    unsafe_code,
    reason = "register a Windows service and create an authenticated local pipe"
)]

use std::collections::BTreeMap;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::Services::*;

static STOP: OnceLock<CancellationToken> = OnceLock::new();
type Leases = Arc<Mutex<BTreeMap<String, (usize, bool)>>>;

pub(super) fn dispatch() -> io::Result<()> {
    let mut name = super::wide(super::SERVICE);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(entry),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

unsafe extern "system" fn control(
    code: u32,
    _: u32,
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) -> u32 {
    match code {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            if let Some(stop) = STOP.get() {
                stop.cancel();
            }
            0
        }
        SERVICE_CONTROL_INTERROGATE => 0,
        _ => 120,
    }
}

unsafe extern "system" fn entry(_: u32, _: *mut *mut u16) {
    let stop = STOP.get_or_init(CancellationToken::new).clone();
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(
            super::wide(super::SERVICE).as_ptr(),
            Some(control),
            ptr::null(),
        )
    };
    if handle.is_null() {
        return;
    }
    let result = (|| {
        report(handle, SERVICE_START_PENDING, 0)?;
        super::journal::Journal::open()?.recover()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let pipe = listener(true)?;
            report(handle, SERVICE_RUNNING, 0)?;
            serve(pipe, stop, handle as usize).await
        })
    })();
    let code = result
        .err()
        .map_or(0, |error| error.raw_os_error().unwrap_or(1) as u32);
    let _ = report(handle, SERVICE_STOPPED, code);
}

fn report(handle: SERVICE_STATUS_HANDLE, state: u32, code: u32) -> io::Result<()> {
    let pending = state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING;
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: code,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: u32::from(pending),
        dwWaitHint: if pending { 30000 } else { 0 },
    };
    if unsafe { SetServiceStatus(handle, &status) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn listener(first: bool) -> io::Result<NamedPipeServer> {
    let mut descriptor = ptr::null_mut();
    let sddl = super::wide("O:SYG:SYD:P(A;;GA;;;SY)(A;;0x12019b;;;AU)");
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
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let result = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                super::PIPE,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    };
    unsafe { LocalFree(descriptor) };
    result
}

async fn serve(
    mut pipe: NamedPipeServer,
    stop: CancellationToken,
    status: usize,
) -> io::Result<()> {
    let leases = Leases::default();
    let mut handlers = JoinSet::new();
    let mut result = Ok(());
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            completed = handlers.join_next(), if !handlers.is_empty() => {
                if let Some(completed) = completed && let Err(error) = completed.map_err(io::Error::other).and_then(|value| value) { result = Err(error); break; }
            }
            connected = pipe.connect(), if handlers.len() < 64 => {
                if let Err(error) = connected { result = Err(error); break; }
                let next = match listener(false) { Ok(next) => next, Err(error) => { result = Err(error); break; } };
                handlers.spawn(connection(pipe, leases.clone(), stop.clone()));
                pipe = next;
            }
        }
    }
    drop(pipe);
    stop.cancel();
    let transition = report(status as SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING, 0);
    while let Some(completed) = handlers.join_next().await {
        result = result.and(completed.map_err(io::Error::other).and_then(|value| value));
    }
    result.and(transition)
}

async fn connection(
    mut pipe: NamedPipeServer,
    leases: Leases,
    stop: CancellationToken,
) -> io::Result<()> {
    let handshake = async {
        let mut magic = [0_u8; 4];
        let mut nonce = [0_u8; 16];
        pipe.read_exact(&mut magic).await?;
        pipe.read_exact(&mut nonce).await?;
        if &magic != super::MAGIC {
            return Err(io::Error::from_raw_os_error(87));
        }
        super::identity::authenticate(pipe.as_raw_handle(), &nonce)
    };
    let profile = tokio::select! {
        () = stop.cancelled() => return Ok(()),
        result = tokio::time::timeout(super::TIMEOUT, handshake) => result.map_err(io::Error::other).and_then(|value| value),
    };
    let profile = match profile {
        Ok(profile) => profile,
        Err(error) => {
            let _ = pipe
                .write_u32_le(error.raw_os_error().unwrap_or(5) as u32)
                .await;
            return Ok(());
        }
    };
    let sid = super::firewall::profile_sid(&profile)?;
    let acquired = change(leases.clone(), sid.clone(), true).await;
    if let Err(error) = acquired {
        let _ = pipe
            .write_u32_le(error.raw_os_error().unwrap_or(5) as u32)
            .await;
        return Ok(());
    }
    if pipe.write_u32_le(0).await.is_ok() {
        tokio::select! {
            () = stop.cancelled() => {},
            _ = pipe.read_u8() => {},
        }
    }
    let released = change(leases, sid, false).await;
    let code = released
        .as_ref()
        .err()
        .map_or(0, |error| error.raw_os_error().unwrap_or(1) as u32);
    let _ = pipe.write_u32_le(code).await;
    released
}

async fn change(leases: Leases, sid: String, acquire: bool) -> io::Result<()> {
    tokio::task::spawn_blocking(move || {
        let mut leases = leases
            .lock()
            .map_err(|error| io::Error::other(format!("sandbox lease state poisoned: {error}")))?;
        let journal = super::journal::Journal::open()?;
        if acquire {
            if let Some((count, _)) = leases.get_mut(&sid) {
                *count += 1;
                return Ok(());
            }
            let owned = journal.contains(&sid)? || !super::firewall::contains(&sid)?;
            if owned {
                journal.add(&sid)?;
                super::firewall::update(&sid, true)?;
            }
            leases.insert(sid, (1, owned));
        } else if let Some((count, owned)) = leases.get_mut(&sid) {
            if *count > 1 {
                *count -= 1;
                return Ok(());
            }
            if *owned {
                super::firewall::update(&sid, false)?;
                journal.remove(&sid)?;
            }
            leases.remove(&sid);
        }
        drop(leases);
        Ok(())
    })
    .await
    .map_err(io::Error::other)?
}
