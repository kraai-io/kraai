use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use kraai_types::SandboxCapability;
use nix::sys::signal::{Signal, kill};
use nix::unistd::{Pid, setsid};
use tokio_util::sync::CancellationToken;

use super::{capabilities, temp_dir};
use crate::{LaunchPlan, Termination, run};

struct DetachedFixture(PathBuf);

impl Drop for DetachedFixture {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(self.0.join("detached.pid"))
            && let Ok(pid) = pid.parse::<i32>()
        {
            let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) async fn detached_output(mode: &str, capability: SandboxCapability, cancelled: bool) {
    let fixture = DetachedFixture(temp_dir("detached-output"));
    std::fs::create_dir(&fixture.0).expect("create detached fixture");
    let mut plan = LaunchPlan::new(
        std::env::current_exe().expect("test executable"),
        fixture.0.clone(),
        capabilities([capability]),
        if cancelled {
            Duration::from_secs(60)
        } else if capability == SandboxCapability::NoSandbox {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(2)
        },
    );
    if capability != SandboxCapability::NoSandbox {
        plan.runtime_roots.push(
            plan.executable
                .parent()
                .expect("test executable parent")
                .to_path_buf(),
        );
        if std::path::Path::new("/nix/store").exists() {
            plan.runtime_roots.push("/nix/store".into());
        }
    }
    plan.args([
        "--exact",
        "tests::process::detached_output_child",
        "--nocapture",
    ]);
    plan.environment = std::env::vars_os().collect();
    plan.environment
        .insert("KRAAI_DETACHED_OUTPUT".into(), mode.into());
    let cancellation = CancellationToken::new();
    if cancelled {
        let token = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            token.cancel();
        });
    }
    let result = tokio::time::timeout(Duration::from_secs(4), run(plan, cancellation))
        .await
        .expect("detached output must not extend the execution timeout")
        .expect("capture detached output");
    assert_eq!(
        result.termination,
        if cancelled {
            Termination::Cancelled
        } else {
            Termination::TimedOut
        }
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("captured stdout"));
    assert!(String::from_utf8_lossy(&result.stderr).contains("captured stderr"));
}

#[tokio::test]
async fn timeout_does_not_wait_for_detached_output_pipes() {
    detached_output("both", SandboxCapability::NoSandbox, false).await;
}

#[tokio::test]
async fn timeout_preserves_completed_stdout_while_stderr_remains_open() {
    detached_output("stderr", SandboxCapability::NoSandbox, false).await;
}

#[tokio::test]
async fn cancellation_preserves_output_from_detached_descendants() {
    detached_output("stderr", SandboxCapability::NoSandbox, true).await;
}

#[test]
fn detached_output_child() {
    let Ok(mode) = std::env::var("KRAAI_DETACHED_OUTPUT") else {
        return;
    };
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "tests::process::detached_pipe_holder",
            "--nocapture",
        ])
        .stdout(if mode == "stderr" {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn detached helper");
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !std::path::Path::new("detached.pid").exists() {
        assert!(std::time::Instant::now() < deadline, "child did not detach");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::io::stdout()
        .write_all(b"captured stdout\n")
        .expect("write captured stdout");
    std::io::stderr()
        .write_all(b"captured stderr\n")
        .expect("write captured stderr");
    let _ = child.try_wait();
}

#[test]
fn detached_pipe_holder() {
    if std::env::var_os("KRAAI_DETACHED_OUTPUT").is_none() {
        return;
    }
    setsid().expect("detach output holder");
    std::fs::write("detached.pid", std::process::id().to_string()).expect("record detached child");
    std::thread::sleep(Duration::from_secs(60));
}
