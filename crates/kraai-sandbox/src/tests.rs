use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kraai_types::{SandboxCapabilities, SandboxCapability};
#[cfg(unix)]
use nix::unistd::Pid;
use tokio_util::sync::CancellationToken;

use crate::{
    BWRAP_SECCOMP_STDIN_FD, LaunchPlan, OutputStream, SandboxError, SeccompInstruction,
    Termination, build_bwrap_args, build_bwrap_probe_args, bwrap_probe_failure_message, find_bwrap,
    is_likely_sandbox_denied, restricted_network_seccomp_program, run, run_bwrap_sandbox_probe,
};

fn temp_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "kraai-sandbox-test-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

fn executable(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH should be set for tests");
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("unable to find test executable '{name}'"))
}

fn capabilities(values: impl IntoIterator<Item = SandboxCapability>) -> SandboxCapabilities {
    SandboxCapabilities::new(values).expect("valid test capabilities")
}

fn shell_plan(
    workspace: &Path,
    script: impl AsRef<OsStr>,
    sandbox_capabilities: SandboxCapabilities,
    timeout: Duration,
) -> LaunchPlan {
    let mut plan = LaunchPlan::new(
        executable("sh"),
        workspace.to_path_buf(),
        sandbox_capabilities,
        timeout,
    );
    plan.args = vec![OsString::from("-c"), script.as_ref().to_os_string()];
    plan.environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
    plan
}

#[test]
fn rejects_relative_executable() {
    let workspace = temp_dir("relative-executable");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let plan = LaunchPlan::new(
        PathBuf::from("sh"),
        workspace.clone(),
        capabilities([SandboxCapability::NoSandbox]),
        Duration::from_secs(1),
    );

    let runtime = tokio::runtime::Runtime::new().expect("create runtime");
    let result = runtime.block_on(run(plan, CancellationToken::new()));
    assert_eq!(result, Err(SandboxError::ExecutableMustBeAbsolute));
    let _ = std::fs::remove_dir_all(workspace);
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
async fn no_sandbox_captures_output_and_clears_unspecified_environment() {
    let workspace = temp_dir("capture");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let mut plan = shell_plan(
        &workspace,
        "printf '%s' \"${KRAAI_UNSET-unset}\"; printf err >&2",
        capabilities([SandboxCapability::NoSandbox]),
        Duration::from_secs(5),
    );
    plan.environment.remove(OsStr::new("KRAAI_UNSET"));

    let output = run(plan, CancellationToken::new())
        .await
        .expect("process should run");
    assert_eq!(output.termination, Termination::Exited { code: Some(0) });
    assert_eq!(output.stdout, b"unset");
    assert_eq!(output.stderr, b"err");
    assert!(!output.sandbox_denied);
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn live_output_events_reconstruct_authoritative_capture() {
    let workspace = temp_dir("events");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut plan = shell_plan(
        &workspace,
        "printf first; printf second; printf problem >&2",
        capabilities([SandboxCapability::NoSandbox]),
        Duration::from_secs(5),
    );
    plan.output_events = Some(sender);

    let output = run(plan, CancellationToken::new())
        .await
        .expect("process should run");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    while let Some(event) = receiver.recv().await {
        match event.stream {
            OutputStream::Stdout => stdout.extend(event.bytes),
            OutputStream::Stderr => stderr.extend(event.bytes),
        }
    }
    assert_eq!(stdout, output.stdout);
    assert_eq!(stderr, output.stderr);
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn private_temp_is_writable_and_removed_after_exit() {
    let workspace = temp_dir("private-temp");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let output = run(
        shell_plan(
            &workspace,
            "printf data > \"$TMPDIR/probe\"; printf %s \"$TMPDIR\"",
            capabilities([SandboxCapability::NoSandbox]),
            Duration::from_secs(5),
        ),
        CancellationToken::new(),
    )
    .await
    .expect("process should run");
    let temp_path = PathBuf::from(String::from_utf8(output.stdout).expect("utf8 temp path"));
    assert!(
        !temp_path.exists(),
        "private temp should be removed after exit"
    );
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
#[cfg(unix)]
async fn timeout_terminates_descendants_and_preserves_output() {
    use nix::sys::signal::kill;

    let workspace = temp_dir("timeout-tree");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let pid_path = workspace.join("grandchild.pid");
    let script = format!(
        "printf started; sleep 60 & child=$!; echo $child > '{}'; wait $child",
        pid_path.display()
    );
    let output = run(
        shell_plan(
            &workspace,
            script,
            capabilities([SandboxCapability::NoSandbox]),
            Duration::from_millis(100),
        ),
        CancellationToken::new(),
    )
    .await
    .expect("timeout is a stable process outcome");

    assert_eq!(output.termination, Termination::TimedOut);
    assert_eq!(output.stdout, b"started");
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
                    "descendant survived"
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
async fn timeout_covers_output_pipes_inherited_after_parent_exit() {
    let workspace = temp_dir("inherited-output-pipe");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let output = run(
        shell_plan(
            &workspace,
            "sleep 60 & printf done",
            capabilities([SandboxCapability::NoSandbox]),
            Duration::from_millis(100),
        ),
        CancellationToken::new(),
    )
    .await
    .expect("timeout is a stable process outcome");
    assert_eq!(output.termination, Termination::TimedOut);
    assert_eq!(output.stdout, b"done");
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn cancellation_returns_capture_and_kills_process() {
    let workspace = temp_dir("cancel");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
    });
    let output = run(
        shell_plan(
            &workspace,
            "printf before; sleep 60",
            capabilities([SandboxCapability::NoSandbox]),
            Duration::from_secs(5),
        ),
        cancellation,
    )
    .await
    .expect("cancellation is a stable process outcome");
    assert_eq!(output.termination, Termination::Cancelled);
    assert_eq!(output.stdout, b"before");
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
#[cfg(target_os = "linux")]
fn bwrap_probe_args_match_network_shapes() {
    let restricted = build_bwrap_probe_args(false);
    assert!(restricted.iter().any(|arg| arg == "--unshare-all"));
    assert!(
        restricted
            .windows(2)
            .any(|window| { window[0] == "--seccomp" && window[1] == BWRAP_SECCOMP_STDIN_FD })
    );
    assert!(!restricted.iter().any(|arg| arg == "--share-net"));

    let enabled = build_bwrap_probe_args(true);
    assert!(enabled.iter().any(|arg| arg == "--share-net"));
    assert!(!enabled.iter().any(|arg| arg == "--seccomp"));
}

#[test]
#[cfg(target_os = "linux")]
fn bwrap_probe_network_namespace_failure_is_actionable() {
    let message = bwrap_probe_failure_message(
        false,
        "Failed to create NETLINK_ROUTE socket: Operation not permitted",
    );
    assert!(message.contains("network namespace"));
    assert!(message.contains("network capability"));
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn bwrap_probe_times_out_instead_of_hanging() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = temp_dir("probe-timeout");
    std::fs::create_dir_all(&workspace).expect("create fake bwrap dir");
    let fake_bwrap = workspace.join("bwrap");
    std::fs::write(&fake_bwrap, "#!/bin/sh\nsleep 10\n").expect("write fake bwrap");
    let mut permissions = std::fs::metadata(&fake_bwrap)
        .expect("fake bwrap metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_bwrap, permissions).expect("make executable");

    let result = run_bwrap_sandbox_probe(&fake_bwrap, false, Duration::from_millis(10)).await;
    assert!(matches!(result, Err(message) if message.contains("timed out")));
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
#[cfg(target_os = "linux")]
fn bwrap_args_encode_capability_boundaries() {
    let workspace = temp_dir("args");
    let private_temp = temp_dir("args-private");
    std::fs::create_dir_all(workspace.join(".kraai")).expect("create metadata dir");
    std::fs::create_dir_all(&private_temp).expect("create private temp");
    let runtime_root = temp_dir("args-runtime-root");
    std::fs::create_dir_all(&runtime_root).expect("create runtime root");
    let mut plan = shell_plan(
        &workspace,
        "true",
        capabilities([
            SandboxCapability::WorkspaceWrite,
            SandboxCapability::Network,
        ]),
        Duration::from_secs(1),
    );
    plan.runtime_roots.push(runtime_root.clone());
    let args = build_bwrap_args(&plan, &private_temp);

    assert!(contains_mount(&args, "--bind", &workspace));
    assert!(contains_mount(
        &args,
        "--ro-bind",
        &workspace.join(".kraai")
    ));
    assert!(contains_mount(&args, "--bind", &private_temp));
    assert!(contains_mount(&args, "--ro-bind", &runtime_root));
    assert!(args.iter().any(|arg| arg == "--share-net"));
    assert!(!args.iter().any(|arg| arg == "--seccomp"));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(private_temp);
    let _ = std::fs::remove_dir_all(runtime_root);
}

#[cfg(target_os = "linux")]
fn contains_mount(args: &[OsString], flag: &str, path: &Path) -> bool {
    args.windows(3).any(|window| {
        window[0] == flag && window[1] == path.as_os_str() && window[2] == path.as_os_str()
    })
}

#[test]
#[cfg(target_os = "linux")]
fn restricted_network_seccomp_program_keeps_unix_socket_creation_but_denies_connect() {
    let program = restricted_network_seccomp_program(&[]).expect("build seccomp program");
    assert!(program.windows(5).any(|window| {
        window[0].k == libc::SYS_socket as u32 && window[2].k == libc::AF_UNIX as u32
    }));
    assert!(program_denies_syscall(&program, libc::SYS_connect as u32));
}

#[test]
#[cfg(target_os = "linux")]
fn restricted_network_seccomp_program_allows_connect_only_on_private_ipc_descriptors() {
    const PRIVATE_DESCRIPTOR: i32 = 20;
    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JMP_JEQ_K: u16 = 0x15;
    let program =
        restricted_network_seccomp_program(&[PRIVATE_DESCRIPTOR]).expect("build seccomp program");

    assert!(program.windows(5).any(|window| {
        window[0].code == BPF_JMP_JEQ_K
            && window[0].k == libc::SYS_connect as u32
            && window[1].code == BPF_LD_W_ABS
            && window[2].code == BPF_JMP_JEQ_K
            && window[2].k == PRIVATE_DESCRIPTOR as u32
            && is_seccomp_errno(&window[3])
            && !is_seccomp_errno(&window[4])
    }));
    assert!(!program_denies_syscall(&program, libc::SYS_connect as u32));
}

#[test]
#[cfg(target_os = "linux")]
fn restricted_network_seccomp_program_denies_socket_message_io() {
    let program = restricted_network_seccomp_program(&[]).expect("build seccomp program");
    for syscall in [
        libc::SYS_sendto,
        libc::SYS_sendmsg,
        libc::SYS_sendmmsg,
        libc::SYS_recvfrom,
        libc::SYS_recvmsg,
        libc::SYS_recvmmsg,
    ] {
        assert!(program_denies_syscall(&program, syscall as u32));
    }
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn restricted_network_seccomp_program_denies_x32_syscalls() {
    let program = restricted_network_seccomp_program(&[]).expect("build seccomp program");
    const BPF_JMP_JGE_K: u16 = 0x35;
    const X32_SYSCALL_BIT: u32 = 0x4000_0000;
    assert!(program.windows(2).any(|window| {
        window[0].code == BPF_JMP_JGE_K
            && window[0].k == X32_SYSCALL_BIT
            && is_seccomp_errno(&window[1])
    }));
}

#[cfg(target_os = "linux")]
fn program_denies_syscall(program: &[SeccompInstruction], syscall: u32) -> bool {
    program
        .windows(2)
        .any(|window| window[0].k == syscall && is_seccomp_errno(&window[1]))
}

#[cfg(target_os = "linux")]
fn is_seccomp_errno(instruction: &SeccompInstruction) -> bool {
    instruction.k & 0xffff_0000 == 0x0005_0000
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn workspace_write_blocks_outside_write_when_supported() {
    let workspace = temp_dir("workspace-boundary");
    let private_temp = temp_dir("bwrap-search");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::create_dir_all(&private_temp).expect("create private temp");
    if find_bwrap(&workspace, &private_temp).is_none() {
        let _ = std::fs::remove_dir_all(workspace);
        let _ = std::fs::remove_dir_all(private_temp);
        return;
    }
    let outside = temp_dir("outside-boundary");
    std::fs::create_dir_all(&outside).expect("create outside");
    let plan = shell_plan(
        &workspace,
        format!(
            "printf ok > allowed; printf denied > '{}'",
            outside.join("denied").display()
        ),
        capabilities([
            SandboxCapability::HostRead,
            SandboxCapability::WorkspaceWrite,
        ]),
        Duration::from_secs(5),
    );

    let output = match run(plan, CancellationToken::new()).await {
        Ok(output) => output,
        Err(SandboxError::SandboxUnavailable(_)) => {
            let _ = std::fs::remove_dir_all(workspace);
            let _ = std::fs::remove_dir_all(private_temp);
            let _ = std::fs::remove_dir_all(outside);
            return;
        }
        Err(error) => panic!("unexpected sandbox error: {error}"),
    };
    assert!(
        workspace.join("allowed").exists(),
        "termination: {:?}, stderr: {}",
        output.termination,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!outside.join("denied").exists());
    assert_ne!(output.termination, Termination::Exited { code: Some(0) });
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(private_temp);
    let _ = std::fs::remove_dir_all(outside);
}
