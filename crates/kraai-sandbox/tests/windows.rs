#![cfg(windows)]
#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::zombie_processes,
    reason = "sandbox tests assert real Windows access decisions"
)]

use std::path::PathBuf;
use std::time::Duration;

use kraai_sandbox::{LaunchPlan, SandboxError, Termination};
use kraai_types::{SandboxCapabilities, SandboxCapability};
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
            SandboxCapabilities::new(capabilities.iter().copied()).expect("valid capabilities"),
            Duration::from_secs(20),
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
    match mode.as_str() {
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
            assert!(std::fs::write(".git/config", "blocked").is_err());
            assert!(std::fs::remove_file(".git/config").is_err());
            assert!(std::fs::rename(".git", "moved-metadata").is_err());
            assert!(std::fs::remove_dir_all(".git").is_err());
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
        "network" => {
            let _listener = std::net::TcpListener::bind("0.0.0.0:0")
                .expect("network capability allows listening");
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
                std::thread::sleep(Duration::from_secs(30));
            }
        }
        "descendant" => {
            std::thread::sleep(Duration::from_secs(2));
            std::fs::write("escaped-child", "survived").expect("write descendant marker");
        }
        "arguments" => {
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

async fn successful(plan: LaunchPlan) {
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
}

#[tokio::test]
async fn isolates_host_reads_writes_network_and_private_temp() {
    let fixture = Fixture::new();
    successful(fixture.plan("readonly", &[SandboxCapability::WorkspaceRead])).await;
    assert!(!fixture.0.join("workspace/blocked").exists());
}

#[tokio::test]
async fn workspace_write_preserves_metadata_and_explicit_metadata_write_works() {
    let fixture = Fixture::new();
    successful(fixture.plan("write", &[SandboxCapability::WorkspaceWrite])).await;
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("workspace/.git/config"))
            .expect("read protected metadata"),
        "metadata"
    );
    successful(fixture.plan("metadata", &[SandboxCapability::MetadataWrite])).await;
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("workspace/.git/config"))
            .expect("read changed metadata"),
        "authorized"
    );
}

#[tokio::test]
async fn rejects_unsupported_host_access_without_running() {
    let fixture = Fixture::new();
    let result = kraai_sandbox::run(
        fixture.plan("metadata", &[SandboxCapability::HostRead]),
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(result, Err(SandboxError::SandboxUnavailable(_))));
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("workspace/.git/config"))
            .expect("metadata remains unchanged"),
        "metadata"
    );
}

#[tokio::test]
async fn captures_output_and_preserves_environment_values() {
    let fixture = Fixture::new();
    let mut plan = fixture.plan("arguments", &[SandboxCapability::WorkspaceRead]);
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
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    plan.output_events = Some(sender);
    let cancellation = CancellationToken::new();
    if !cancel {
        plan.timeout = Duration::from_secs(1);
    }
    let execution = tokio::spawn(kraai_sandbox::run(plan, cancellation.clone()));
    wait_for_descendant(events).await;
    if cancel {
        cancellation.cancel();
    }
    let output = execution
        .await
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
async fn network_capability_allows_sockets() {
    let fixture = Fixture::new();
    successful(fixture.plan(
        "network",
        &[SandboxCapability::WorkspaceRead, SandboxCapability::Network],
    ))
    .await;
}

#[tokio::test]
async fn normal_exit_kills_remaining_descendants() {
    let fixture = Fixture::new();
    successful(fixture.plan("tree-exit", &[SandboxCapability::WorkspaceWrite])).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!fixture.0.join("workspace/escaped-child").exists());
}

#[tokio::test]
async fn dropping_execution_kills_descendants() {
    let fixture = Fixture::new();
    let mut plan = fixture.plan("tree", &[SandboxCapability::WorkspaceWrite]);
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    plan.output_events = Some(sender);
    let execution = tokio::spawn(kraai_sandbox::run(plan, CancellationToken::new()));
    wait_for_descendant(events).await;
    execution.abort();
    assert!(execution.await.expect_err("task cancelled").is_cancelled());
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!fixture.0.join("workspace/escaped-child").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn aborted_cleanup_does_not_block_the_runtime() {
    let fixture = Fixture::new();
    let mut plan = fixture.plan("tree", &[SandboxCapability::WorkspaceWrite]);
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
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
        elapsed < Duration::from_millis(500),
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
    let wait = unsafe { WaitForSingleObject(handle.as_raw_handle(), 15000) };
    assert!(wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED);
    let _ = ready.send(());
    let _ = release.recv_timeout(Duration::from_secs(2));
    assert_ne!(unsafe { ReleaseMutex(handle.as_raw_handle()) }, 0);
}

async fn wait_for_descendant(
    mut events: tokio::sync::mpsc::UnboundedReceiver<kraai_sandbox::OutputEvent>,
) {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut output = Vec::new();
        while let Some(event) = events.recv().await {
            output.extend(event.bytes);
            if String::from_utf8_lossy(&output).contains("descendant started") {
                return;
            }
        }
        panic!("execution exited before starting descendant");
    })
    .await
    .expect("host started");
}
