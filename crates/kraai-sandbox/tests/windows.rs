#![cfg(windows)]
// Clippy only applies its allow-*-in-tests settings to helpers under a `cfg(test)` ancestor.
#![cfg(test)]
#![expect(
    clippy::zombie_processes,
    reason = "sandbox tests assert real Windows access decisions"
)]

use std::path::PathBuf;
use std::time::Duration;

use kraai_execution_types::{SandboxCapabilities, SandboxCapability};
use kraai_sandbox::{LaunchPlan, SandboxError, Termination};
use kraai_test_support::{HANG_TIMEOUT, hang_guard};
use tokio_util::sync::CancellationToken;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "kraai-windows-test-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir_all(path.join("workspace/.git")).expect("create workspace");
        std::fs::write(path.join("workspace/input"), "input").expect("write input");
        std::fs::write(path.join("workspace/.git/config"), "metadata").expect("write metadata");
        std::fs::write(path.join("secret"), "host secret").expect("write host secret");
        Self(path)
    }

    fn plan(&self, mode: &str, capabilities: &[SandboxCapability]) -> LaunchPlan {
        let executable = std::env::current_exe().expect("locate test executable");
        let mut plan = LaunchPlan::new(
            executable.clone(),
            self.0.join("workspace"),
            SandboxCapabilities::new(capabilities.iter().copied()),
            HANG_TIMEOUT,
        );
        plan.runtime_roots.push(executable);
        plan.args(["--exact", "probe", "--nocapture"]);
        plan.environment
            .insert("KRAAI_WINDOWS_PROBE".into(), mode.into());
        plan.environment.insert(
            "KRAAI_WINDOWS_SECRET".into(),
            self.0.join("secret").into_os_string(),
        );
        plan
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn probe() {
    let Ok(mode) = std::env::var("KRAAI_WINDOWS_PROBE") else {
        return;
    };
    println!("package-sid={}", package_sid());
    match mode.as_str() {
        "temp-owner" => {
            let file = std::env::temp_dir().join("owned");
            std::fs::write(&file, "private temp").expect("private temp write");
            println!("temp-file={}", file.display());
            while !std::path::Path::new("release").exists() {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        "target-readable" => {
            let target = std::env::var_os("KRAAI_WINDOWS_TARGET").expect("target path");
            std::fs::read(target).expect("granted target read");
        }
        "target-denied" => {
            let target =
                PathBuf::from(std::env::var_os("KRAAI_WINDOWS_TARGET").expect("target path"));
            assert!(std::fs::read(&target).is_err());
            let parent = target.parent().expect("target parent");
            assert!(std::fs::read_dir(parent).is_err());
        }
        "readonly" => {
            assert_eq!(
                std::fs::read_to_string("input").expect("workspace read"),
                "input"
            );
            assert!(std::fs::write("blocked", "write").is_err());
            let secret = std::env::var_os("KRAAI_WINDOWS_SECRET").expect("secret path");
            assert!(std::fs::read(secret).is_err());
            assert!(std::net::TcpListener::bind("0.0.0.0:0").is_err());
            std::fs::write(std::env::temp_dir().join("scratch"), "private temp")
                .expect("private temp write");
        }
        "write" => {
            std::fs::write("created", "workspace write").expect("workspace write");
            std::fs::remove_file("input").expect("delete ordinary file");
            std::fs::write(".git/config", "authorized").expect("write repository config");
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{WRITE_DAC, WRITE_OWNER};
            for access in [WRITE_DAC, WRITE_OWNER] {
                assert!(
                    std::fs::OpenOptions::new()
                        .access_mode(access)
                        .open(".git/config")
                        .is_err()
                );
            }
        }
        "metadata" => {
            std::fs::write(".git/config", "authorized").expect("metadata write");
        }
        "tree" | "tree-exit" => {
            let executable = std::env::current_exe().expect("current executable");
            let _child = std::process::Command::new(executable)
                .args(["--exact", "probe", "--nocapture"])
                .env("KRAAI_WINDOWS_PROBE", "descendant")
                .spawn()
                .expect("spawn descendant");
            println!("descendant started");
            if mode == "tree" {
                std::thread::sleep(HANG_TIMEOUT * 2);
            }
        }
        "descendant" => {
            while !std::path::Path::new("parent-finished").exists() {
                std::thread::sleep(Duration::from_millis(20));
            }
            std::thread::sleep(Duration::from_secs(2));
            std::fs::write("escaped-child", "survived").expect("write descendant marker");
        }
        "arguments" => {
            assert_eq!(
                std::env::var("=C:").expect("hidden drive environment"),
                r"C:\work"
            );
            assert_eq!(
                std::env::var("MiXeD").expect("case insensitive environment"),
                "spaces and unicode \u{1f426}"
            );
            println!("stdout marker");
            eprintln!("stderr marker");
        }
        _ => panic!("unknown probe mode {mode}"),
    }
}

#[expect(
    unsafe_code,
    reason = "the probe reads its AppContainer SID from its own token"
)]
fn package_sid() -> String {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_APPCONTAINER_INFORMATION, TOKEN_QUERY, TokenAppContainerSid,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = std::ptr::null_mut();
    assert_ne!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
        0
    );
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut buffer = vec![0_usize; 64];
    let mut length = 0;
    assert_ne!(
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenAppContainerSid,
                buffer.as_mut_ptr().cast(),
                size_of_val(buffer.as_slice()) as u32,
                &mut length,
            )
        },
        0
    );
    let information = unsafe {
        buffer
            .as_ptr()
            .cast::<TOKEN_APPCONTAINER_INFORMATION>()
            .read()
    };
    assert!(!information.TokenAppContainer.is_null());
    let mut string = std::ptr::null_mut();
    assert_ne!(
        unsafe { ConvertSidToStringSidW(information.TokenAppContainer, &mut string) },
        0
    );
    let mut length = 0;
    while unsafe { *string.add(length) } != 0 {
        length += 1;
    }
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(string, length) });
    unsafe { LocalFree(string.cast()) };
    sid
}

fn field(output: &str, name: &str) -> String {
    output
        .lines()
        .find_map(|line| line.trim_end().strip_prefix(name)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("probe output lacks {name}: {output}"))
        .to_owned()
}

async fn successful(plan: LaunchPlan) -> String {
    let output = kraai_sandbox::run(plan, CancellationToken::new())
        .await
        .expect("Windows sandbox must launch");
    assert_eq!(
        output.termination,
        Termination::Exited { code: Some(0) },
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test]
async fn workspace_write_allows_repository_changes() {
    let fixture = Fixture::new();
    successful(fixture.plan("write", &[SandboxCapability::WorkspaceWrite])).await;
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("workspace/.git/config"))
            .expect("read repository config"),
        "authorized"
    );
}

#[tokio::test]
async fn rejects_unsupported_host_access_without_running() {
    let fixture = Fixture::new();
    for capabilities in [
        &[SandboxCapability::HostRead][..],
        &[
            SandboxCapability::HostRead,
            SandboxCapability::WorkspaceWrite,
        ][..],
        &[SandboxCapability::HostWrite][..],
    ] {
        for network in [false, true] {
            let mut capabilities = capabilities.to_vec();
            if network {
                capabilities.push(SandboxCapability::Network);
            }
            let result = kraai_sandbox::run(
                fixture.plan("metadata", &capabilities),
                CancellationToken::new(),
            )
            .await;
            assert!(
                matches!(result, Err(SandboxError::SandboxUnavailable(_))),
                "unsupported capabilities {capabilities:?}"
            );
        }
    }
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("workspace/.git/config"))
            .expect("metadata remains unchanged"),
        "metadata"
    );
}

#[tokio::test]
async fn concurrent_launches_preserve_workspace_isolation() {
    let mut launches = tokio::task::JoinSet::new();
    for _ in 0..4 {
        launches.spawn(async {
            let fixture = Fixture::new();
            let (first, second) = tokio::join!(
                successful(fixture.plan("readonly", &[SandboxCapability::WorkspaceRead])),
                successful(fixture.plan("readonly", &[SandboxCapability::WorkspaceRead])),
            );
            [field(&first, "package-sid"), field(&second, "package-sid")]
        });
    }
    let mut identities = std::collections::HashSet::new();
    while let Some(result) = launches.join_next().await {
        identities.extend(result.expect("concurrent launch task"));
    }
    assert_eq!(identities.len(), 8, "{identities:?}");
}

#[tokio::test]
async fn concurrent_launches_in_one_workspace_keep_private_temp_private() {
    let fixture = Fixture::new();
    let mut owner = fixture.plan("temp-owner", &[SandboxCapability::WorkspaceRead]);
    let (sender, events) = tokio::sync::mpsc::channel(16);
    owner.output_events = Some(sender);
    let owner = tokio::spawn(kraai_sandbox::run(owner, CancellationToken::new()));
    let started = wait_for_output(events, "temp-file=").await;
    let mut intruder = fixture.plan("target-denied", &[SandboxCapability::WorkspaceRead]);
    intruder.environment.insert(
        "KRAAI_WINDOWS_TARGET".into(),
        field(&started, "temp-file").into(),
    );
    let intruder = successful(intruder).await;
    std::fs::write(fixture.0.join("workspace/release"), "").expect("release temp owner");
    let owner = hang_guard(owner)
        .await
        .expect("temp owner finished")
        .expect("join temp owner")
        .expect("run temp owner");
    assert_eq!(owner.termination, Termination::Exited { code: Some(0) });
    assert_ne!(
        field(&started, "package-sid"),
        field(&intruder, "package-sid")
    );
}

#[tokio::test]
async fn runtime_roots_stay_closed_to_launches_without_them() {
    let fixture = Fixture::new();
    let root = fixture.0.join("runtime");
    std::fs::create_dir(&root).expect("create runtime root");
    std::fs::write(root.join("data"), "runtime data").expect("write runtime data");
    let mut granted = fixture.plan("target-readable", &[SandboxCapability::WorkspaceRead]);
    granted.runtime_roots.push(root.clone());
    granted
        .environment
        .insert("KRAAI_WINDOWS_TARGET".into(), root.join("data").into());
    successful(granted).await;
    let mut denied = fixture.plan("target-denied", &[SandboxCapability::WorkspaceRead]);
    denied
        .environment
        .insert("KRAAI_WINDOWS_TARGET".into(), root.join("data").into());
    successful(denied).await;
}

#[tokio::test]
async fn captures_output_and_preserves_environment_values() {
    let fixture = Fixture::new();
    let mut plan = fixture.plan("arguments", &[SandboxCapability::WorkspaceRead]);
    plan.environment.insert("=C:".into(), r"C:\work".into());
    plan.environment
        .insert("MiXeD".into(), "spaces and unicode \u{1f426}".into());
    let output = kraai_sandbox::run(plan, CancellationToken::new())
        .await
        .expect("launch");
    assert_eq!(output.termination, Termination::Exited { code: Some(0) });
    assert!(String::from_utf8_lossy(&output.stdout).contains("stdout marker"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("stderr marker"));
}

async fn assert_tree_stopped(fixture: &Fixture, cancel: bool) {
    let mut plan = fixture.plan("tree", &[SandboxCapability::WorkspaceWrite]);
    let (sender, events) = tokio::sync::mpsc::channel(16);
    plan.output_events = Some(sender);
    let cancellation = CancellationToken::new();
    if !cancel {
        plan.timeout = Duration::from_secs(5);
    }
    let (started, execution_started) = tokio::sync::oneshot::channel();
    plan.execution_started = Some(execution_started);
    let execution = tokio::spawn(kraai_sandbox::run(plan, cancellation.clone()));
    wait_for_descendant(events).await;
    if cancel {
        cancellation.cancel();
    } else {
        let _ = started.send(tokio::time::Instant::now());
    }
    let output = hang_guard(execution)
        .await
        .expect("execution finished")
        .expect("join execution")
        .expect("run execution");
    assert_eq!(
        output.termination,
        if cancel {
            Termination::Cancelled
        } else {
            Termination::TimedOut
        }
    );
    std::fs::write(fixture.0.join("workspace/parent-finished"), "")
        .expect("release descendant probe");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!fixture.0.join("workspace/escaped-child").exists());
}

#[tokio::test]
async fn timeout_kills_descendants() {
    assert_tree_stopped(&Fixture::new(), false).await;
}

#[tokio::test]
async fn cancellation_kills_descendants() {
    assert_tree_stopped(&Fixture::new(), true).await;
}

#[tokio::test]
async fn normal_exit_kills_remaining_descendants() {
    let fixture = Fixture::new();
    successful(fixture.plan("tree-exit", &[SandboxCapability::WorkspaceWrite])).await;
    std::fs::write(fixture.0.join("workspace/parent-finished"), "")
        .expect("release descendant probe");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!fixture.0.join("workspace/escaped-child").exists());
}

#[tokio::test]
async fn dropping_execution_kills_descendants() {
    let fixture = Fixture::new();
    let mut plan = fixture.plan("tree", &[SandboxCapability::WorkspaceWrite]);
    let (sender, events) = tokio::sync::mpsc::channel(16);
    plan.output_events = Some(sender);
    let execution = tokio::spawn(kraai_sandbox::run(plan, CancellationToken::new()));
    wait_for_descendant(events).await;
    execution.abort();
    assert!(execution.await.expect_err("task cancelled").is_cancelled());
    std::fs::write(fixture.0.join("workspace/parent-finished"), "")
        .expect("release descendant probe");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!fixture.0.join("workspace/escaped-child").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn aborted_cleanup_does_not_block_the_runtime() {
    let fixture = Fixture::new();
    let mut plan = fixture.plan("tree", &[SandboxCapability::WorkspaceWrite]);
    let (sender, events) = tokio::sync::mpsc::channel(16);
    plan.output_events = Some(sender);
    let execution = tokio::spawn(kraai_sandbox::run(plan, CancellationToken::new()));
    wait_for_descendant(events).await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = tokio::task::spawn_blocking(move || hold_cleanup_lock(ready_tx, release_rx));
    ready_rx.await.expect("cleanup lock held");
    let started = std::time::Instant::now();
    execution.abort();
    assert!(
        execution
            .await
            .expect_err("execution cancelled")
            .is_cancelled()
    );
    tokio::time::sleep(Duration::from_millis(10)).await;
    let elapsed = started.elapsed();
    let _ = release_tx.send(());
    holder.await.expect("lock holder finished");
    assert!(
        elapsed < HANG_TIMEOUT,
        "cleanup blocked the runtime for {elapsed:?}"
    );
}

#[expect(
    unsafe_code,
    reason = "the regression test holds the same Windows mutex used by ACL cleanup"
)]
fn hold_cleanup_lock(
    ready: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
) {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

    let name = "Global\\KraaiSandboxAclMutation"
        .encode_utf16()
        .chain([0])
        .collect::<Vec<_>>();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    assert!(!handle.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    let timeout = u32::try_from(HANG_TIMEOUT.as_millis()).expect("hang timeout fits a wait");
    let wait = unsafe { WaitForSingleObject(handle.as_raw_handle(), timeout) };
    assert!(wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED);
    let _ = ready.send(());
    let _ = release.recv_timeout(HANG_TIMEOUT * 2);
    assert_ne!(unsafe { ReleaseMutex(handle.as_raw_handle()) }, 0);
}

async fn wait_for_descendant(events: tokio::sync::mpsc::Receiver<kraai_sandbox::OutputEvent>) {
    wait_for_output(events, "descendant started").await;
}

async fn wait_for_output(
    mut events: tokio::sync::mpsc::Receiver<kraai_sandbox::OutputEvent>,
    marker: &str,
) -> String {
    hang_guard(async {
        let mut output = Vec::new();
        while let Some(event) = events.recv().await {
            output.extend(event.bytes);
            let text = String::from_utf8_lossy(&output);
            if let Some(start) = text.find(marker)
                && text.get(start..).is_some_and(|rest| rest.contains('\n'))
            {
                return text.into_owned();
            }
        }
        panic!("execution exited before printing {marker}");
    })
    .await
    .expect("host started")
}
