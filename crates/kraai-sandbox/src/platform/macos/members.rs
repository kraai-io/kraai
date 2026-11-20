#![expect(
    unsafe_code,
    reason = "Seatbelt membership checks and the process table are only exposed by libsystem"
)]

use std::collections::HashSet;
use std::ffi::{CStr, CString, c_char, c_int};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use crate::SandboxError;

const MACH_LOOKUP: &CStr = c"mach-lookup";
const SANDBOX_FILTER_GLOBAL_NAME: c_int = 2;
const TERMINATION_TIMEOUT: Duration = Duration::from_secs(5);

unsafe extern "C" {
    fn sandbox_check(pid: libc::pid_t, operation: *const c_char, filter: c_int, ...) -> c_int;
    static SANDBOX_CHECK_NO_REPORT: c_int;
}

pub(crate) async fn terminate(run_marker: String) -> Result<(), SandboxError> {
    tokio::task::spawn_blocking(move || terminate_blocking(&run_marker))
        .await
        .map_err(|error| SandboxError::Wait(format!("sandbox process cleanup failed: {error}")))?
}

pub(crate) fn terminate_blocking(run_marker: &str) -> Result<(), SandboxError> {
    let marker = c_name(run_marker)?;
    let decoy = c_name(&super::profile::random_mach_name()?)?;
    let deadline = Instant::now() + TERMINATION_TIMEOUT;
    let mut exited = HashSet::new();
    loop {
        if kill_members(&marker, &decoy, &mut exited)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(SandboxError::Wait(format!(
                "sandboxed processes were still running {} seconds after termination",
                TERMINATION_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn c_name(name: &str) -> Result<CString, SandboxError> {
    CString::new(name).map_err(|error| SandboxError::Wait(error.to_string()))
}

/// Signals every member and returns whether the pass proved none are left. A listed process that
/// exits before its membership check may have forked a member this pass never listed, so a
/// pass only counts once every process that exited during it was already gone before.
fn kill_members(
    marker: &CStr,
    decoy: &CStr,
    exited: &mut HashSet<libc::pid_t>,
) -> Result<bool, SandboxError> {
    let own_pid = std::process::id();
    let mut settled = true;
    for pid in all_pids()? {
        if pid <= 1 || u32::try_from(pid) == Ok(own_pid) {
            continue;
        }
        let ProcessState::Live(started) = process_state(pid) else {
            settled &= !exited.insert(pid);
            continue;
        };
        let member = is_member(pid, marker, decoy);
        if process_state(pid) != ProcessState::Live(started) {
            settled &= !exited.insert(pid);
            continue;
        }
        if !member {
            continue;
        }
        match kill(Pid::from_raw(pid), Signal::SIGKILL) {
            Ok(()) | Err(Errno::EPERM) => settled = false,
            Err(Errno::ESRCH) => settled &= !exited.insert(pid),
            Err(error) => {
                return Err(SandboxError::Wait(format!(
                    "unable to terminate sandboxed process {pid}: {error}"
                )));
            }
        }
    }
    Ok(settled)
}

fn is_member(pid: libc::pid_t, marker: &CStr, decoy: &CStr) -> bool {
    // SAFETY: the symbol is an immutable integer exported by libsystem_sandbox.
    let filter = SANDBOX_FILTER_GLOBAL_NAME | unsafe { SANDBOX_CHECK_NO_REPORT };
    // SAFETY: both strings are NUL-terminated and outlive the call, and a global-name filter
    // takes exactly one C string argument.
    let check =
        |name: &CStr| unsafe { sandbox_check(pid, MACH_LOOKUP.as_ptr(), filter, name.as_ptr()) };
    check(marker) == 0 && check(decoy) > 0
}

#[derive(PartialEq, Eq)]
enum ProcessState {
    Gone,
    Live(Option<(u64, u64)>),
}

fn process_state(pid: libc::pid_t) -> ProcessState {
    // SAFETY: `proc_bsdinfo` contains only integers and byte arrays, so all-zero is valid.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let Ok(size) = c_int::try_from(size_of::<libc::proc_bsdinfo>()) else {
        return ProcessState::Live(None);
    };
    // SAFETY: the buffer is a live `proc_bsdinfo` of exactly `size` bytes.
    let written =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    if written == size {
        return if info.pbi_status == libc::SZOMB {
            ProcessState::Gone
        } else {
            ProcessState::Live(Some((info.pbi_start_tvsec, info.pbi_start_tvusec)))
        };
    }
    match Errno::last() {
        Errno::ESRCH => ProcessState::Gone,
        _ => ProcessState::Live(None),
    }
}

fn all_pids() -> Result<Vec<libc::pid_t>, SandboxError> {
    // SAFETY: a null buffer asks only for the current number of processes.
    let mut estimate = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    loop {
        let slots = usize::try_from(estimate).map_err(|_overflow| list_error())? + 256;
        let mut pids = vec![0 as libc::pid_t; slots];
        let bytes = c_int::try_from(slots * size_of::<libc::pid_t>())
            .map_err(|error| SandboxError::Wait(error.to_string()))?;
        // SAFETY: the buffer holds `bytes` bytes of writable PID slots.
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if count <= 0 {
            return Err(list_error());
        }
        let count = usize::try_from(count).map_err(|_overflow| list_error())?;
        if count < slots {
            pids.truncate(count);
            return Ok(pids);
        }
        estimate =
            c_int::try_from(slots * 2).map_err(|error| SandboxError::Wait(error.to_string()))?;
    }
}

fn list_error() -> SandboxError {
    SandboxError::Wait(format!(
        "unable to list processes: {}",
        std::io::Error::last_os_error()
    ))
}
