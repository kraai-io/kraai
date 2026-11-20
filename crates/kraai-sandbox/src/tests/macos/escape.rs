use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kraai_execution_types::SandboxCapability;
use kraai_test_support::{HANG_TIMEOUT, hang_guard};
use nix::sys::signal::{Signal, kill};
use nix::unistd::{Pid, setsid};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{Fixture, capabilities};
use crate::{ExecutionOutput, LaunchPlan, SandboxError, Termination, run};

const PROBE: &str = "tests::macos::escape::escape_child";
const ROLE: &str = "KRAAI_ESCAPE_ROLE";
const KIND: &str = "KRAAI_ESCAPE_KIND";
const FINISH: &str = "KRAAI_ESCAPE_FINISH";
const DEPTH: &str = "KRAAI_ESCAPE_DEPTH";
const FORK_DEPTH: u32 = 3;
const FORK_WIDTH: u32 = 3;

struct EscapeFixture(Fixture);

impl EscapeFixture {
    fn new(name: &str) -> Self {
        let fixture = Fixture::new(name);
        for directory in ["pids", "escaped"] {
            std::fs::create_dir(fixture.workspace().join(directory)).expect("create record dir");
        }
        Self(fixture)
    }

    fn workspace(&self) -> PathBuf {
        self.0.workspace()
    }
}

impl Drop for EscapeFixture {
    fn drop(&mut self) {
        for pid in recorded(&self.workspace(), "pids") {
            let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        }
    }
}

fn recorded(workspace: &Path, directory: &str) -> Vec<i32> {
    std::fs::read_dir(workspace.join(directory))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn spawn_role(role: &str, depth: u32) -> std::process::Child {
    Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", PROBE, "--nocapture"])
        .env(ROLE, role)
        .env(DEPTH, depth.to_string())
        .spawn()
        .expect("spawn escape helper")
}

fn wait_for_records(directory: &str, count: usize) {
    let deadline = Instant::now() + HANG_TIMEOUT;
    while std::fs::read_dir(directory).map_or(0, Iterator::count) < count {
        assert!(Instant::now() < deadline, "escape helpers did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn hold_until_released() {
    let pid = std::process::id();
    std::fs::write(format!("pids/{pid}"), "").expect("record escaped process");
    let deadline = Instant::now() + HANG_TIMEOUT * 2;
    while Instant::now() < deadline {
        if Path::new("release").exists() {
            std::fs::write(format!("escaped/{pid}"), "").expect("record survival");
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn escape_child() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let depth = std::env::var(DEPTH)
        .ok()
        .and_then(|depth| depth.parse().ok())
        .unwrap_or(0);
    match role.as_str() {
        "main" => {
            let kind = std::env::var(KIND).expect("escape kind");
            let mut child = spawn_role(&kind, 0);
            wait_for_records("pids", if kind == "forker" { 4 } else { 1 });
            std::io::stdout()
                .write_all(b"ready\n")
                .expect("announce escape");
            std::io::stdout().flush().expect("flush announcement");
            let _ = child.try_wait();
            if std::env::var(FINISH).as_deref() != Ok("exit") {
                std::thread::sleep(HANG_TIMEOUT * 2);
            }
        }
        "detach" => {
            let _ = spawn_role("holder", 0).try_wait();
        }
        "holder" => {
            setsid().expect("leave the process group and session");
            hold_until_released();
        }
        "forker" => {
            setsid().expect("leave the process group and session");
            let mut children = Vec::new();
            if depth < FORK_DEPTH {
                for _ in 0..FORK_WIDTH {
                    std::thread::sleep(Duration::from_millis(10));
                    children.push(spawn_role("forker", depth + 1));
                }
            }
            hold_until_released();
        }
        role => panic!("unknown escape role {role}"),
    }
}

struct Running {
    execution: JoinHandle<Result<ExecutionOutput, SandboxError>>,
    cancellation: CancellationToken,
    started: tokio::sync::oneshot::Sender<tokio::time::Instant>,
}

async fn start(workspace: &Path, kind: &str, finish: &str) -> Running {
    let executable = std::env::current_exe().expect("test executable");
    let mut plan = LaunchPlan::new(
        executable.clone(),
        workspace.to_path_buf(),
        capabilities([SandboxCapability::WorkspaceWrite]),
        Duration::from_secs(1),
    );
    plan.args = ["--exact", PROBE, "--nocapture"]
        .into_iter()
        .map(OsString::from)
        .collect();
    plan.runtime_roots = vec![
        executable
            .parent()
            .expect("test binary parent")
            .to_path_buf(),
    ];
    if Path::new("/nix/store").exists() {
        plan.runtime_roots.push(PathBuf::from("/nix/store"));
    }
    plan.environment.insert(ROLE.into(), "main".into());
    plan.environment.insert(KIND.into(), kind.into());
    plan.environment.insert(FINISH.into(), finish.into());
    let (sender, mut events) = tokio::sync::mpsc::channel(16);
    plan.output_events = Some(sender);
    let (started, execution_started) = tokio::sync::oneshot::channel();
    plan.execution_started = Some(execution_started);
    let cancellation = CancellationToken::new();
    let execution = tokio::spawn(run(plan, cancellation.clone()));
    hang_guard(async move {
        let mut output = Vec::new();
        while let Some(event) = events.recv().await {
            output.extend(event.bytes);
            if output.windows(5).any(|bytes| bytes == b"ready") {
                return;
            }
        }
        panic!("sandboxed process exited before its descendants escaped");
    })
    .await
    .expect("escaped descendants started");
    Running {
        execution,
        cancellation,
        started,
    }
}

async fn finish(running: Running, mode: &str) {
    match mode {
        "cancel" => running.cancellation.cancel(),
        "timeout" => {
            let _ = running.started.send(tokio::time::Instant::now());
        }
        "drop" => {
            running.execution.abort();
            assert!(
                running
                    .execution
                    .await
                    .expect_err("task aborted")
                    .is_cancelled()
            );
            return;
        }
        _ => {}
    }
    let output = hang_guard(running.execution)
        .await
        .expect("execution finished")
        .expect("execution task")
        .expect("sandboxed run");
    assert_eq!(
        output.termination,
        match mode {
            "exit" => Termination::Exited { code: Some(0) },
            "cancel" => Termination::Cancelled,
            _ => Termination::TimedOut,
        },
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn assert_none_survived(workspace: &Path, label: &str) {
    assert!(
        !recorded(workspace, "pids").is_empty(),
        "{label}: nothing escaped"
    );
    std::fs::write(workspace.join("release"), "").expect("release survivors");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        recorded(workspace, "escaped").is_empty(),
        "{label}: escaped sandbox processes survived"
    );
}

async fn assert_survives(workspace: &Path, label: &str) {
    std::fs::write(workspace.join("release"), "").expect("release survivor");
    hang_guard(async {
        while recorded(workspace, "escaped").is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_elapsed| panic!("{label} was killed by another run's cleanup"));
}

#[tokio::test]
async fn detached_sessions_stop_on_every_completion_path() {
    for mode in ["exit", "timeout", "cancel", "drop"] {
        let fixture = EscapeFixture::new("macos-escape");
        let running = start(&fixture.workspace(), "detach", mode).await;
        finish(running, mode).await;
        assert_none_survived(&fixture.workspace(), mode).await;
    }
}

#[tokio::test]
async fn fork_loops_of_detached_sessions_are_fully_killed() {
    let fixture = EscapeFixture::new("macos-fork-loop");
    let running = start(&fixture.workspace(), "forker", "exit").await;
    finish(running, "exit").await;
    assert_none_survived(&fixture.workspace(), "fork loop").await;
}

#[tokio::test]
async fn cleanup_spares_other_runs_and_unsandboxed_processes() {
    let other = EscapeFixture::new("macos-escape-other");
    let other_run = start(&other.workspace(), "detach", "wait").await;

    let host = EscapeFixture::new("macos-escape-host");
    let mut unsandboxed = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", PROBE, "--nocapture"])
        .env(ROLE, "holder")
        .current_dir(host.workspace())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn unsandboxed process");
    hang_guard(async {
        while recorded(&host.workspace(), "pids").is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("unsandboxed process started");

    let target = EscapeFixture::new("macos-escape-target");
    let running = start(&target.workspace(), "detach", "exit").await;
    finish(running, "exit").await;
    assert_none_survived(&target.workspace(), "target run").await;

    assert_survives(&other.workspace(), "a concurrent run").await;
    assert_survives(&host.workspace(), "an unsandboxed process").await;
    finish(other_run, "cancel").await;
    let _ = unsandboxed.kill();
    let _ = unsandboxed.wait();
}
