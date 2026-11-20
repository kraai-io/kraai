use crate::error::SandboxError;

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;
const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
const LANDLOCK_ACCESS_FS_RESOLVE_UNIX: u64 = 1 << 16;
const LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
const HOST_IPC_ABI: i64 = 9;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

pub(crate) fn ensure_host_ipc_isolation_available() -> Result<(), SandboxError> {
    let abi = landlock_abi();
    if abi >= HOST_IPC_ABI {
        return Ok(());
    }
    let found = if abi > 0 {
        format!("this kernel provides Landlock ABI {abi}")
    } else {
        String::from("Landlock is unavailable on this kernel")
    };
    Err(SandboxError::SandboxUnavailable(format!(
        "the network capability needs Landlock ABI {HOST_IPC_ABI} or newer to keep sandboxed scripts away from host sockets, but {found}; request no-sandbox to run this script"
    )))
}

/// Denies connections to UNIX sockets created outside this process and its descendants,
/// once the host holds its trusted IPC connection. Sockets created later stay reachable.
#[expect(
    unsafe_code,
    reason = "Landlock is configured through raw syscalls with a live attribute struct"
)]
pub fn restrict_host_ipc_after_startup() -> Result<(), SandboxError> {
    ensure_host_ipc_isolation_available()?;
    let attr = RulesetAttr {
        // Every Landlock ruleset that handles a filesystem right also denies moving and
        // linking files between directories, unless a rule allows it.
        handled_access_fs: LANDLOCK_ACCESS_FS_RESOLVE_UNIX | LANDLOCK_ACCESS_FS_REFER,
        handled_access_net: 0,
        scoped: LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET,
    };
    // SAFETY: the attribute struct stays alive for the call and its size matches the
    // kernel ABI that defines every field used here.
    let ruleset = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0_u32,
        )
    };
    if ruleset < 0 {
        return Err(landlock_error("create Landlock ruleset"));
    }
    let ruleset = i32::try_from(ruleset).map_err(|error| {
        SandboxError::SandboxUnavailable(format!("invalid Landlock ruleset descriptor: {error}"))
    })?;
    // SAFETY: the kernel returned this descriptor, and nothing else owns it.
    let ruleset = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(ruleset) };
    allow_refer_everywhere(&ruleset)?;
    // SAFETY: prctl takes scalar arguments here.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(landlock_error("set no_new_privs"));
    }
    // SAFETY: the ruleset descriptor is open for the duration of the call.
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_restrict_self,
            std::os::fd::AsRawFd::as_raw_fd(&ruleset),
            0_u32,
        )
    };
    if result != 0 {
        return Err(landlock_error("enforce Landlock ruleset"));
    }
    Ok(())
}

#[expect(
    unsafe_code,
    reason = "Landlock rules are added through a raw syscall with a live attribute struct"
)]
fn allow_refer_everywhere(ruleset: &std::os::fd::OwnedFd) -> Result<(), SandboxError> {
    let root = std::fs::File::open("/").map_err(|error| {
        SandboxError::SandboxUnavailable(format!("unable to open / for Landlock: {error}"))
    })?;
    let rule = PathBeneathAttr {
        allowed_access: LANDLOCK_ACCESS_FS_REFER,
        parent_fd: std::os::fd::AsRawFd::as_raw_fd(&root),
    };
    // SAFETY: the rule and both descriptors stay alive for the call.
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            std::os::fd::AsRawFd::as_raw_fd(ruleset),
            LANDLOCK_RULE_PATH_BENEATH,
            &rule as *const PathBeneathAttr,
            0_u32,
        )
    };
    if result != 0 {
        return Err(landlock_error("allow moving files in the Landlock ruleset"));
    }
    Ok(())
}

#[expect(
    unsafe_code,
    reason = "querying the Landlock ABI version uses a raw syscall"
)]
fn landlock_abi() -> i64 {
    // SAFETY: a null attribute with zero size and the version flag only queries the ABI.
    unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0_usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    }
}

fn landlock_error(action: &str) -> SandboxError {
    SandboxError::SandboxUnavailable(format!(
        "unable to {action}: {}",
        std::io::Error::last_os_error()
    ))
}
