#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
#[cfg(target_os = "linux")]
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use kraai_types::{NetworkAccess, SandboxConfig, SandboxMode, SandboxPermissions};
#[cfg(unix)]
use nix::sys::signal::{Signal, killpg};
#[cfg(unix)]
use nix::unistd::Pid;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const BWRAP_PROGRAM: &str = "bwrap";
#[cfg(target_os = "linux")]
const BWRAP_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const BWRAP_SECCOMP_STDIN_FD: &str = "0";
const PROTECTED_METADATA_NAMES: &[&str] = &[".git", ".jj", ".kraai", ".agents", ".codex"];

#[cfg(target_os = "linux")]
type BwrapProbeCache = BTreeMap<(PathBuf, bool), Result<(), String>>;

#[cfg(target_os = "linux")]
static BWRAP_PROBE_CACHE: OnceLock<Mutex<BwrapProbeCache>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct CommandRequest {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub sandbox: SandboxConfig,
    pub sandbox_permissions: SandboxPermissions,
    pub timeout: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub sandbox_denied: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CommandError {
    EmptyCommand,
    InvalidTimeout,
    UnsupportedSandboxPermission(SandboxPermissions),
    SandboxUnavailable(String),
    Spawn { program: String, message: String },
    Wait(String),
    TimedOut(Duration),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyCommand => write!(f, "command must contain at least one item"),
            Self::InvalidTimeout => write!(f, "timeout must be greater than zero"),
            Self::UnsupportedSandboxPermission(permission) => write!(
                f,
                "sandbox permission '{}' is not supported yet",
                permission.as_str()
            ),
            Self::SandboxUnavailable(message) => write!(f, "sandbox unavailable: {message}"),
            Self::Spawn { program, message } => {
                write!(f, "unable to spawn command '{program}': {message}")
            }
            Self::Wait(message) => write!(f, "unable to wait for command: {message}"),
            Self::TimedOut(timeout) => {
                write!(f, "command timed out after {} second(s)", timeout.as_secs())
            }
        }
    }
}

impl std::error::Error for CommandError {}

pub async fn run_command(request: CommandRequest) -> Result<CommandOutput, CommandError> {
    if request.timeout.is_zero() {
        return Err(CommandError::InvalidTimeout);
    }

    run_command_with_timeout(&request).await
}

async fn run_command_with_timeout(request: &CommandRequest) -> Result<CommandOutput, CommandError> {
    let command = prepare_command(request).await?;
    spawn_and_wait(command, request.timeout).await
}

async fn prepare_command(request: &CommandRequest) -> Result<PreparedCommand, CommandError> {
    let Some(program) = request.command.first() else {
        return Err(CommandError::EmptyCommand);
    };

    match request.sandbox_permissions {
        SandboxPermissions::UseDefault => {}
        SandboxPermissions::RequireEscalated => {
            return Ok(PreparedCommand::unsandboxed(
                program.clone(),
                request.command.get(1..).unwrap_or_default().to_vec(),
                request.cwd.clone(),
            ));
        }
        SandboxPermissions::WithAdditionalPermissions => {
            return Err(CommandError::UnsupportedSandboxPermission(
                request.sandbox_permissions,
            ));
        }
    }

    if request.sandbox.mode == SandboxMode::DangerFullAccess {
        return Ok(PreparedCommand::unsandboxed(
            program.clone(),
            request.command.get(1..).unwrap_or_default().to_vec(),
            request.cwd.clone(),
        ));
    }

    prepare_sandboxed_command(request).await
}

async fn spawn_and_wait(
    command: PreparedCommand,
    timeout: Duration,
) -> Result<CommandOutput, CommandError> {
    let PreparedCommand {
        program,
        args,
        cwd,
        sandboxed,
        #[cfg(target_os = "linux")]
        seccomp_filter,
    } = command;

    let stdin = {
        #[cfg(target_os = "linux")]
        {
            seccomp_filter.map_or_else(Stdio::null, Stdio::from)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Stdio::null()
        }
    };

    let mut process = Command::new(&program);
    process
        .args(&args)
        .current_dir(&cwd)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_process_tree(&mut process, &program)?;

    let mut child = process.spawn().map_err(|error| CommandError::Spawn {
        program: program.to_string_lossy().into_owned(),
        message: error.to_string(),
    })?;
    let process_group_id = child.id();
    let mut stdout = child.stdout.take().ok_or_else(|| {
        CommandError::Wait(String::from(
            "spawned command did not provide a stdout pipe",
        ))
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| {
        CommandError::Wait(String::from(
            "spawned command did not provide a stderr pipe",
        ))
    })?;
    let stdout_task = tokio::spawn(async move {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).await.map(|_| output)
    });
    let stderr_task = tokio::spawn(async move {
        let mut output = Vec::new();
        stderr.read_to_end(&mut output).await.map(|_| output)
    });

    let completed = async {
        let status = child
            .wait()
            .await
            .map_err(|error| CommandError::Wait(error.to_string()))?;
        let stdout = stdout_task
            .await
            .map_err(|error| CommandError::Wait(error.to_string()))?
            .map_err(|error| CommandError::Wait(error.to_string()))?;
        let stderr = stderr_task
            .await
            .map_err(|error| CommandError::Wait(error.to_string()))?
            .map_err(|error| CommandError::Wait(error.to_string()))?;
        Ok::<_, CommandError>((status, stdout, stderr))
    };
    let (status, stdout, stderr) = match tokio::time::timeout(timeout, completed).await {
        Ok(result) => result?,
        Err(_) => {
            terminate_process_tree(&mut child, process_group_id).await?;
            return Err(CommandError::TimedOut(timeout));
        }
    };

    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    let exit_code = status.code();
    let sandbox_denied = sandboxed && is_likely_sandbox_denied(exit_code, &stdout, &stderr);

    Ok(CommandOutput {
        exit_code,
        stdout,
        stderr,
        sandbox_denied,
    })
}

#[cfg(unix)]
fn configure_process_tree(process: &mut Command, _program: &OsString) -> Result<(), CommandError> {
    use std::os::unix::process::CommandExt;

    process.as_std_mut().process_group(0);
    Ok(())
}

#[cfg(not(unix))]
fn configure_process_tree(_process: &mut Command, _program: &OsString) -> Result<(), CommandError> {
    Ok(())
}

#[cfg(unix)]
async fn terminate_process_tree(
    child: &mut tokio::process::Child,
    process_group_id: Option<u32>,
) -> Result<(), CommandError> {
    if let Some(process_group_id) = process_group_id {
        let process_group_id = i32::try_from(process_group_id)
            .map_err(|error| CommandError::Wait(error.to_string()))?;
        if let Err(error) = killpg(Pid::from_raw(process_group_id), Signal::SIGKILL)
            && error != nix::errno::Errno::ESRCH
        {
            return Err(CommandError::Wait(format!(
                "unable to terminate command process group: {error}"
            )));
        }
    } else {
        child
            .kill()
            .await
            .map_err(|error| CommandError::Wait(error.to_string()))?;
    }
    child
        .wait()
        .await
        .map_err(|error| CommandError::Wait(error.to_string()))?;
    Ok(())
}

#[cfg(not(unix))]
async fn terminate_process_tree(
    child: &mut tokio::process::Child,
    _process_group_id: Option<u32>,
) -> Result<(), CommandError> {
    child
        .kill()
        .await
        .map_err(|error| CommandError::Wait(error.to_string()))?;
    child
        .wait()
        .await
        .map_err(|error| CommandError::Wait(error.to_string()))?;
    Ok(())
}

#[derive(Debug)]
struct PreparedCommand {
    program: OsString,
    args: Vec<String>,
    cwd: PathBuf,
    sandboxed: bool,
    #[cfg(target_os = "linux")]
    seccomp_filter: Option<File>,
}

impl PreparedCommand {
    fn unsandboxed(program: String, args: Vec<String>, cwd: PathBuf) -> Self {
        Self {
            program: OsString::from(program),
            args,
            cwd,
            sandboxed: false,
            #[cfg(target_os = "linux")]
            seccomp_filter: None,
        }
    }
}

#[cfg(target_os = "linux")]
async fn prepare_sandboxed_command(
    request: &CommandRequest,
) -> Result<PreparedCommand, CommandError> {
    let bwrap = find_bwrap(&request.cwd, &request.sandbox).ok_or_else(|| {
        CommandError::SandboxUnavailable(String::from(
            "bubblewrap was not found on PATH; install bubblewrap or use require_escalated",
        ))
    })?;
    ensure_bwrap_sandbox_available(&bwrap, request.sandbox.network_access).await?;
    let seccomp_filter = restricted_network_seccomp_filter(request.sandbox.network_access)?;
    let args = build_bwrap_args(
        &request.command,
        &request.cwd,
        &request.sandbox,
        request.sandbox.network_access,
    );

    Ok(PreparedCommand {
        program: bwrap.into_os_string(),
        args,
        cwd: request.cwd.clone(),
        sandboxed: true,
        seccomp_filter,
    })
}

#[cfg(not(target_os = "linux"))]
async fn prepare_sandboxed_command(
    _request: &CommandRequest,
) -> Result<PreparedCommand, CommandError> {
    Err(CommandError::SandboxUnavailable(String::from(
        "sandboxed command execution is currently only implemented on Linux",
    )))
}

#[cfg(target_os = "linux")]
async fn ensure_bwrap_sandbox_available(
    bwrap: &Path,
    network_access: NetworkAccess,
) -> Result<(), CommandError> {
    let key = (bwrap.to_path_buf(), !network_access.is_enabled());
    let cache = BWRAP_PROBE_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));

    {
        let guard = cache.lock().map_err(|error| {
            CommandError::SandboxUnavailable(format!(
                "bubblewrap sandbox capability cache is unavailable: {error}"
            ))
        })?;
        if let Some(result) = guard.get(&key).cloned() {
            return result.map_err(CommandError::SandboxUnavailable);
        }
    }

    let result = run_bwrap_sandbox_probe(bwrap, network_access, BWRAP_PROBE_TIMEOUT).await;
    cache
        .lock()
        .map_err(|error| {
            CommandError::SandboxUnavailable(format!(
                "bubblewrap sandbox capability cache is unavailable: {error}"
            ))
        })?
        .insert(key, result.clone());
    result.map_err(CommandError::SandboxUnavailable)
}

#[cfg(target_os = "linux")]
async fn run_bwrap_sandbox_probe(
    bwrap: &Path,
    network_access: NetworkAccess,
    timeout: Duration,
) -> Result<(), String> {
    let mut process = Command::new(bwrap);
    let seccomp_filter = restricted_network_seccomp_filter(network_access)
        .map_err(|error| format!("unable to prepare bubblewrap seccomp probe: {error}"))?;
    let stdin = seccomp_filter.map_or_else(Stdio::null, Stdio::from);
    process
        .args(build_bwrap_probe_args(network_access))
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = process
        .spawn()
        .map_err(|error| format!("unable to run bubblewrap sandbox probe: {error}"))?;
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(format!(
                "unable to wait for bubblewrap sandbox probe: {error}"
            ));
        }
        Err(_) => return Err(bwrap_probe_timeout_message(timeout)),
    };

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(bwrap_probe_failure_message(network_access, &stderr))
}

#[cfg(target_os = "linux")]
fn bwrap_probe_timeout_message(timeout: Duration) -> String {
    format!(
        "bubblewrap sandbox probe timed out after {} second(s)",
        timeout.as_secs()
    )
}

#[cfg(target_os = "linux")]
fn build_bwrap_probe_args(network_access: NetworkAccess) -> Vec<String> {
    let mut args = vec![
        String::from("--new-session"),
        String::from("--die-with-parent"),
        String::from("--ro-bind"),
        String::from("/"),
        String::from("/"),
        String::from("--dev"),
        String::from("/dev"),
        String::from("--unshare-user"),
        String::from("--unshare-pid"),
    ];

    if !network_access.is_enabled() {
        args.push(String::from("--unshare-net"));
        args.push(String::from("--seccomp"));
        args.push(String::from(BWRAP_SECCOMP_STDIN_FD));
    }

    args.push(String::from("--chdir"));
    args.push(String::from("/"));
    args.push(String::from("--"));
    args.push(String::from("true"));
    args
}

#[cfg(target_os = "linux")]
fn bwrap_probe_failure_message(network_access: NetworkAccess, stderr: &str) -> String {
    if !network_access.is_enabled() && is_network_namespace_error(stderr) {
        return String::from(
            "bubblewrap cannot create a network namespace on this host; use require_escalated or enable sandbox network access",
        );
    }

    let detail = stderr.trim();
    if detail.is_empty() {
        return String::from("bubblewrap sandbox probe failed");
    }
    format!("bubblewrap sandbox probe failed: {detail}")
}

#[cfg(target_os = "linux")]
fn is_network_namespace_error(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("netlink_route")
        || lower.contains("network namespace")
        || lower.contains("newnet")
        || lower.contains("unshare-net")
}

#[cfg(target_os = "linux")]
fn restricted_network_seccomp_filter(
    network_access: NetworkAccess,
) -> Result<Option<File>, CommandError> {
    if network_access.is_enabled() {
        return Ok(None);
    }

    create_seccomp_filter_file().map(Some)
}

#[cfg(target_os = "linux")]
fn create_seccomp_filter_file() -> Result<File, CommandError> {
    let mut file = create_temp_seccomp_file()?;
    write_seccomp_filter(&mut file)?;
    file.seek(SeekFrom::Start(0))
        .map_err(seccomp_file_error("rewind seccomp filter file"))?;
    Ok(file)
}

#[cfg(target_os = "linux")]
fn create_temp_seccomp_file() -> Result<File, CommandError> {
    let base = std::env::temp_dir();
    for attempt in 0..16_u8 {
        let path = base.join(format!(
            "kraai-bwrap-seccomp-{}-{}-{attempt}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                let _ = std::fs::remove_file(path);
                return Ok(file);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(CommandError::SandboxUnavailable(format!(
                    "unable to create seccomp filter file: {error}"
                )));
            }
        }
    }

    Err(CommandError::SandboxUnavailable(String::from(
        "unable to create unique seccomp filter file",
    )))
}

#[cfg(target_os = "linux")]
fn seccomp_file_error(
    action: &'static str,
) -> impl FnOnce(std::io::Error) -> CommandError + 'static {
    move |error| CommandError::SandboxUnavailable(format!("unable to {action}: {error}"))
}

#[cfg(target_os = "linux")]
fn write_seccomp_filter(file: &mut File) -> Result<(), CommandError> {
    for instruction in restricted_network_seccomp_program()? {
        file.write_all(&instruction.code.to_ne_bytes())
            .map_err(seccomp_file_error("write seccomp filter instruction"))?;
        file.write_all(&[instruction.jt, instruction.jf])
            .map_err(seccomp_file_error("write seccomp filter instruction"))?;
        file.write_all(&instruction.k.to_ne_bytes())
            .map_err(seccomp_file_error("write seccomp filter instruction"))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct SeccompInstruction {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[cfg(target_os = "linux")]
fn stmt(code: u16, k: u32) -> SeccompInstruction {
    SeccompInstruction {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

#[cfg(target_os = "linux")]
fn jump(code: u16, k: u32, jt: u8, jf: u8) -> SeccompInstruction {
    SeccompInstruction { code, jt, jf, k }
}

#[cfg(target_os = "linux")]
fn restricted_network_seccomp_program() -> Result<Vec<SeccompInstruction>, CommandError> {
    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JMP_JEQ_K: u16 = 0x15;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_DATA_NR_OFFSET: u32 = 0;
    const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const DENY: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    let Some(audit_arch) = audit_arch() else {
        return Err(CommandError::SandboxUnavailable(String::from(
            "restricted-network seccomp is unsupported on this CPU architecture",
        )));
    };

    let mut program = vec![
        stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARCH_OFFSET),
        jump(BPF_JMP_JEQ_K, audit_arch, 1, 0),
        stmt(BPF_RET_K, DENY),
        stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET),
    ];
    append_arch_syscall_rejections(&mut program);

    append_af_unix_only_socket_rule(&mut program, libc::SYS_socket as u32);
    append_af_unix_only_socket_rule(&mut program, libc::SYS_socketpair as u32);

    for syscall in [
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_connect,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_getpeername,
        libc::SYS_getsockname,
        libc::SYS_shutdown,
        libc::SYS_sendto,
        libc::SYS_sendmsg,
        libc::SYS_sendmmsg,
        libc::SYS_recvfrom,
        libc::SYS_recvmsg,
        libc::SYS_recvmmsg,
        libc::SYS_getsockopt,
        libc::SYS_setsockopt,
    ] {
        append_deny_syscall_rule(&mut program, syscall as u32);
    }

    program.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
    Ok(program)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn append_arch_syscall_rejections(program: &mut Vec<SeccompInstruction>) {
    const BPF_JMP_JGE_K: u16 = 0x35;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const X32_SYSCALL_BIT: u32 = 0x4000_0000;
    const DENY: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    program.push(jump(BPF_JMP_JGE_K, X32_SYSCALL_BIT, 0, 1));
    program.push(stmt(BPF_RET_K, DENY));
}

#[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
fn append_arch_syscall_rejections(_program: &mut Vec<SeccompInstruction>) {}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn audit_arch() -> Option<u32> {
    Some(0xc000_003e)
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn audit_arch() -> Option<u32> {
    Some(0xc000_00b7)
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
fn audit_arch() -> Option<u32> {
    None
}

#[cfg(target_os = "linux")]
fn append_af_unix_only_socket_rule(program: &mut Vec<SeccompInstruction>, syscall: u32) {
    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JMP_JEQ_K: u16 = 0x15;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_DATA_ARG0_OFFSET: u32 = 16;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const DENY: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    program.push(jump(BPF_JMP_JEQ_K, syscall, 0, 4));
    program.push(stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARG0_OFFSET));
    program.push(jump(BPF_JMP_JEQ_K, libc::AF_UNIX as u32, 1, 0));
    program.push(stmt(BPF_RET_K, DENY));
    program.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
}

#[cfg(target_os = "linux")]
fn append_deny_syscall_rule(program: &mut Vec<SeccompInstruction>, syscall: u32) {
    const BPF_JMP_JEQ_K: u16 = 0x15;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const DENY: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    program.push(jump(BPF_JMP_JEQ_K, syscall, 0, 1));
    program.push(stmt(BPF_RET_K, DENY));
}

#[cfg(target_os = "linux")]
fn build_bwrap_args(
    command: &[String],
    cwd: &Path,
    sandbox: &SandboxConfig,
    network_access: NetworkAccess,
) -> Vec<String> {
    let mut args = vec![
        String::from("--new-session"),
        String::from("--die-with-parent"),
        String::from("--ro-bind"),
        String::from("/"),
        String::from("/"),
        String::from("--dev"),
        String::from("/dev"),
    ];

    let writable_roots = writable_roots(cwd, sandbox);
    for root in &writable_roots {
        push_bind(&mut args, "--bind", root.as_path());
    }

    if sandbox.mode == SandboxMode::WorkspaceWrite {
        for root in &writable_roots {
            for protected in protected_metadata_paths(root.as_path()) {
                push_bind(&mut args, "--ro-bind", protected.as_path());
            }
        }
    }

    args.push(String::from("--unshare-user"));
    args.push(String::from("--unshare-pid"));

    if !network_access.is_enabled() {
        args.push(String::from("--unshare-net"));
        args.push(String::from("--seccomp"));
        args.push(String::from(BWRAP_SECCOMP_STDIN_FD));
    }

    args.push(String::from("--chdir"));
    args.push(path_to_string(cwd));
    args.push(String::from("--"));
    args.extend(command.iter().cloned());
    args
}

#[cfg(target_os = "linux")]
fn writable_roots(cwd: &Path, sandbox: &SandboxConfig) -> Vec<PathBuf> {
    if sandbox.mode != SandboxMode::WorkspaceWrite {
        return Vec::new();
    }

    let mut roots = BTreeSet::new();
    roots.insert(cwd.to_path_buf());
    roots.extend(sandbox.writable_roots.iter().cloned());
    roots.extend(default_tmp_writable_roots());

    let mut roots = roots
        .into_iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    roots.sort_by_key(|path| path.components().count());
    roots
}

#[cfg(target_os = "linux")]
fn default_tmp_writable_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/tmp")];
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        roots.push(PathBuf::from(tmpdir));
    }
    roots
}

#[cfg(target_os = "linux")]
fn protected_metadata_paths(root: &Path) -> Vec<PathBuf> {
    PROTECTED_METADATA_NAMES
        .iter()
        .map(|name| root.join(name))
        .filter(|path| path.exists())
        .collect()
}

#[cfg(target_os = "linux")]
fn push_bind(args: &mut Vec<String>, flag: &str, path: &Path) {
    let path = path_to_string(path);
    args.push(String::from(flag));
    args.push(path.clone());
    args.push(path);
}

fn is_likely_sandbox_denied(exit_code: Option<i32>, stdout: &str, stderr: &str) -> bool {
    if exit_code == Some(0) {
        return false;
    }

    const SANDBOX_DENIED_KEYWORDS: [&str; 8] = [
        "operation not permitted",
        "permission denied",
        "no permissions",
        "read-only file system",
        "seccomp",
        "sandbox",
        "landlock",
        "failed to write file",
    ];

    [stdout, stderr].into_iter().any(|section| {
        let lower = section.to_lowercase();
        SANDBOX_DENIED_KEYWORDS
            .iter()
            .any(|keyword| lower.contains(keyword))
    })
}

#[cfg(target_os = "linux")]
fn find_bwrap(workspace_dir: &Path, sandbox: &SandboxConfig) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let cwd = std::env::current_dir().ok()?;
    let mut disallowed_roots = vec![workspace_dir.to_path_buf()];
    disallowed_roots.extend(sandbox.writable_roots.iter().cloned());
    disallowed_roots.extend(default_tmp_writable_roots());
    let disallowed_roots = disallowed_roots
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect::<Vec<_>>();

    std::env::split_paths(&path_var).find_map(|entry| {
        let base = if entry.is_absolute() {
            entry
        } else {
            cwd.join(entry)
        };
        let candidate = base.join(BWRAP_PROGRAM);
        if !candidate.is_file() {
            return None;
        }
        let canonical = candidate.canonicalize().unwrap_or(candidate);
        if disallowed_roots
            .iter()
            .any(|root| canonical.starts_with(root))
        {
            return None;
        }
        Some(canonical)
    })
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "command runner tests assert argv construction and process behavior directly"
)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "kraai-command-runner-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn sandbox_denial_keywords_are_detected() {
        assert!(is_likely_sandbox_denied(
            Some(1),
            "",
            "Read-only file system"
        ));
        assert!(!is_likely_sandbox_denied(Some(0), "", "permission denied"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn timeout_terminates_background_descendants() {
        use nix::sys::signal::kill;

        let workspace = temp_dir("timeout-process-tree");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let pid_path = workspace.join("grandchild.pid");
        let script = format!(
            "sleep 60 & child=$!; echo $child > '{}'; wait $child",
            pid_path.display()
        );

        let result = run_command(CommandRequest {
            command: vec![String::from("sh"), String::from("-c"), script],
            cwd: workspace.clone(),
            sandbox: SandboxConfig {
                mode: SandboxMode::DangerFullAccess,
                ..SandboxConfig::default()
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            timeout: Duration::from_millis(100),
        })
        .await;

        assert_eq!(
            result,
            Err(CommandError::TimedOut(Duration::from_millis(100)))
        );
        let pid = std::fs::read_to_string(&pid_path)
            .expect("read grandchild pid")
            .trim()
            .parse::<i32>()
            .expect("parse grandchild pid");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            match kill(Pid::from_raw(pid), None) {
                Err(nix::errno::Errno::ESRCH) => break,
                Ok(()) | Err(nix::errno::Errno::EPERM) => {
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "background descendant {pid} survived timeout"
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("unexpected process lookup error: {error}"),
            }
        }

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn timeout_includes_draining_inherited_pipes() {
        let workspace = temp_dir("timeout-inherited-pipes");
        std::fs::create_dir_all(&workspace).expect("create workspace");

        let result = run_command(CommandRequest {
            command: vec![
                String::from("sh"),
                String::from("-c"),
                String::from("sleep 60 & printf done"),
            ],
            cwd: workspace.clone(),
            sandbox: SandboxConfig {
                mode: SandboxMode::DangerFullAccess,
                ..SandboxConfig::default()
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            timeout: Duration::from_millis(100),
        })
        .await;

        assert_eq!(
            result,
            Err(CommandError::TimedOut(Duration::from_millis(100)))
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn bwrap_probe_args_match_restricted_network_shape() {
        let args = build_bwrap_probe_args(NetworkAccess::Restricted);

        assert!(args.iter().any(|arg| arg == "--unshare-net"));
        assert!(
            args.windows(2)
                .any(|window| { window[0] == "--seccomp" && window[1] == BWRAP_SECCOMP_STDIN_FD })
        );
        assert!(
            args.windows(3)
                .any(|window| { window[0] == "--ro-bind" && window[1] == "/" && window[2] == "/" })
        );
        assert!(
            args.windows(2)
                .any(|window| { window[0] == "--chdir" && window[1] == "/" })
        );
        assert_eq!(args[args.len() - 2], "--");
        assert_eq!(args[args.len() - 1], "true");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn bwrap_probe_args_leave_network_enabled_when_requested() {
        let args = build_bwrap_probe_args(NetworkAccess::Enabled);

        assert!(!args.iter().any(|arg| arg == "--unshare-net"));
        assert!(!args.iter().any(|arg| arg == "--seccomp"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn bwrap_probe_network_namespace_failure_is_actionable() {
        let message = bwrap_probe_failure_message(
            NetworkAccess::Restricted,
            "Failed to create NETLINK_ROUTE socket: Operation not permitted",
        );

        assert!(message.contains("network namespace"));
        assert!(message.contains("require_escalated"));
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn bwrap_probe_times_out_instead_of_hanging() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = temp_dir("bwrap-probe-timeout");
        std::fs::create_dir_all(&workspace).expect("create fake bwrap dir");

        let fake_bwrap = workspace.join("bwrap");
        std::fs::write(&fake_bwrap, "#!/bin/sh\nsleep 10\n").expect("write fake bwrap");
        let mut permissions = std::fs::metadata(&fake_bwrap)
            .expect("fake bwrap metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_bwrap, permissions).expect("make fake bwrap executable");

        let result = run_bwrap_sandbox_probe(
            &fake_bwrap,
            NetworkAccess::Restricted,
            Duration::from_millis(10),
        )
        .await;

        assert!(matches!(result, Err(message) if message.contains("timed out")));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn bwrap_args_make_workspace_writable_and_metadata_read_only() {
        let workspace = temp_dir("args");
        std::fs::create_dir_all(workspace.join(".kraai")).expect("create metadata dir");
        let sandbox = SandboxConfig::default();

        let args = build_bwrap_args(
            &[String::from("true")],
            &workspace,
            &sandbox,
            NetworkAccess::Restricted,
        );

        assert!(args.windows(3).any(|window| {
            window[0] == "--bind"
                && window[1] == workspace.to_string_lossy()
                && window[2] == workspace.to_string_lossy()
        }));
        assert!(args.windows(3).any(|window| {
            window[0] == "--ro-bind"
                && window[1] == workspace.join(".kraai").to_string_lossy()
                && window[2] == workspace.join(".kraai").to_string_lossy()
        }));
        assert!(args.iter().any(|arg| arg == "--unshare-net"));
        assert!(
            args.windows(2)
                .any(|window| { window[0] == "--seccomp" && window[1] == BWRAP_SECCOMP_STDIN_FD })
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn restricted_network_seccomp_program_keeps_unix_socket_creation_but_denies_connect() {
        let program = restricted_network_seccomp_program().expect("build seccomp program");
        let socket_syscall = libc::SYS_socket as u32;

        assert!(
            program
                .windows(5)
                .any(|window| window[0].k == socket_syscall && window[2].k == libc::AF_UNIX as u32)
        );
        assert!(program_denies_syscall(&program, libc::SYS_connect as u32));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn restricted_network_seccomp_program_denies_socket_message_io() {
        let program = restricted_network_seccomp_program().expect("build seccomp program");

        for syscall in [
            libc::SYS_sendto,
            libc::SYS_sendmsg,
            libc::SYS_sendmmsg,
            libc::SYS_recvfrom,
            libc::SYS_recvmsg,
            libc::SYS_recvmmsg,
        ] {
            assert!(
                program_denies_syscall(&program, syscall as u32),
                "syscall {syscall} should be denied"
            );
        }
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn restricted_network_seccomp_program_denies_x32_syscalls() {
        let program = restricted_network_seccomp_program().expect("build seccomp program");
        const BPF_JMP_JGE_K: u16 = 0x35;
        const X32_SYSCALL_BIT: u32 = 0x4000_0000;

        assert!(
            program
                .windows(2)
                .any(|window| window[0].code == BPF_JMP_JGE_K
                    && window[0].k == X32_SYSCALL_BIT
                    && is_seccomp_errno(&window[1]))
        );
    }

    #[cfg(target_os = "linux")]
    fn program_denies_syscall(program: &[SeccompInstruction], syscall: u32) -> bool {
        program
            .windows(2)
            .any(|window| window[0].k == syscall && is_seccomp_errno(&window[1]))
    }

    #[cfg(target_os = "linux")]
    fn is_seccomp_errno(instruction: &SeccompInstruction) -> bool {
        const SECCOMP_RET_ACTION_FULL: u32 = 0xffff_0000;
        const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;

        instruction.k & SECCOMP_RET_ACTION_FULL == SECCOMP_RET_ERRNO
    }

    #[tokio::test]
    async fn require_escalated_runs_without_sandbox() {
        let workspace = temp_dir("escalated");
        std::fs::create_dir_all(&workspace).expect("create workspace");

        let output = run_command(CommandRequest {
            command: vec![
                String::from("sh"),
                String::from("-c"),
                String::from("printf ok"),
            ],
            cwd: workspace.clone(),
            sandbox: SandboxConfig::default(),
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            timeout: Duration::from_secs(5),
        })
        .await
        .expect("command should run");

        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "ok");
        assert!(!output.sandbox_denied);

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn workspace_write_sandbox_blocks_parent_write_when_supported() {
        let workspace = temp_dir("sandbox-workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");

        let sandbox = SandboxConfig::default();
        if find_bwrap(&workspace, &sandbox).is_none() {
            let _ = std::fs::remove_dir_all(workspace);
            return;
        }

        let probe = run_command(CommandRequest {
            command: vec![String::from("true")],
            cwd: workspace.clone(),
            sandbox: sandbox.clone(),
            sandbox_permissions: SandboxPermissions::UseDefault,
            timeout: Duration::from_secs(5),
        })
        .await;
        if !matches!(probe, Ok(output) if output.exit_code == Some(0)) {
            let _ = std::fs::remove_dir_all(workspace);
            return;
        }

        let outside = std::env::current_dir()
            .expect("current dir")
            .join("target/kraai-command-runner-tests")
            .join(format!(
                "sandbox-outside-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&outside).expect("create outside");

        let output = run_command(CommandRequest {
            command: vec![
                String::from("sh"),
                String::from("-c"),
                format!(
                    "printf ok > allowed && printf no > {}",
                    outside.join("denied").display()
                ),
            ],
            cwd: workspace.clone(),
            sandbox,
            sandbox_permissions: SandboxPermissions::UseDefault,
            timeout: Duration::from_secs(5),
        })
        .await;

        let output = match output {
            Ok(output) => output,
            Err(CommandError::SandboxUnavailable(_)) => {
                let _ = std::fs::remove_dir_all(workspace);
                let _ = std::fs::remove_dir_all(outside);
                return;
            }
            Err(error) => panic!("unexpected command error: {error}"),
        };

        assert!(workspace.join("allowed").exists());
        assert!(!outside.join("denied").exists());
        assert!(output.exit_code != Some(0));
        assert!(output.sandbox_denied);

        let _ = std::fs::remove_dir_all(workspace);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn restricted_network_sandbox_blocks_unix_socket_connect_when_supported() {
        use std::os::unix::net::UnixListener;

        let workspace = temp_dir("sandbox-unix-socket");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let socket_path = workspace.join("agent.sock");
        let listener = match UnixListener::bind(&socket_path) {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                let _ = std::fs::remove_dir_all(workspace);
                return;
            }
            Err(error) => panic!("bind unix listener: {error}"),
        };

        let sandbox = SandboxConfig::default();
        if find_bwrap(&workspace, &sandbox).is_none() {
            let _ = std::fs::remove_file(&socket_path);
            let _ = std::fs::remove_dir_all(workspace);
            return;
        }

        let exe = std::env::current_exe().expect("current test executable");
        let command = format!(
            "KRAAI_COMMAND_RUNNER_UNIX_SOCKET_PROBE='{}' '{}' --exact tests::unix_socket_connect_probe --ignored --nocapture",
            socket_path.display(),
            exe.display()
        );
        let output = run_command(CommandRequest {
            command: vec![String::from("sh"), String::from("-c"), command],
            cwd: workspace.clone(),
            sandbox,
            sandbox_permissions: SandboxPermissions::UseDefault,
            timeout: Duration::from_secs(5),
        })
        .await;

        drop(listener);

        let output = match output {
            Ok(output) => output,
            Err(CommandError::SandboxUnavailable(_)) => {
                let _ = std::fs::remove_file(&socket_path);
                let _ = std::fs::remove_dir_all(workspace);
                return;
            }
            Err(error) => panic!("unexpected command error: {error}"),
        };

        assert_eq!(output.exit_code, Some(0), "stderr: {}", output.stderr);

        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    #[ignore]
    #[cfg(target_os = "linux")]
    fn unix_socket_connect_probe() {
        use std::os::unix::net::UnixStream;

        let path =
            std::env::var("KRAAI_COMMAND_RUNNER_UNIX_SOCKET_PROBE").expect("probe socket path env");
        match UnixStream::connect(path) {
            Ok(_) => panic!("unix socket connect unexpectedly succeeded"),
            Err(error) => assert_eq!(error.raw_os_error(), Some(libc::EPERM)),
        }
    }
}
