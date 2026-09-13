use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use nix::unistd::{Pid, close, setpgid};
use tokio_util::sync::CancellationToken;

use super::{DetachedFixture, capabilities, temp_dir};
use crate::{LaunchPlan, Termination, run};
use kraai_types::SandboxCapability;

const PROBE: &str = "tests::process::group_change::changing_group_child";
const MODE: &str = "KRAAI_GROUP_CHANGE";

#[test]
fn changing_group_child() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    if mode == "holder" {
        std::thread::sleep(Duration::from_secs(10));
        return;
    }
    let mut holder = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", PROBE, "--nocapture"])
        .env(MODE, "holder")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn another process group");
    let pid = i32::try_from(holder.id()).expect("child PID");
    std::fs::write("detached.pid", pid.to_string()).expect("record helper for cleanup");
    setpgid(Pid::from_raw(0), Pid::from_raw(pid)).expect("main process joins another group");
    std::io::stdout()
        .write_all(b"group changed\n")
        .expect("announce group change");
    std::io::stdout().flush().expect("flush announcement");
    close(libc::STDOUT_FILENO).expect("close stdout");
    close(libc::STDERR_FILENO).expect("close stderr");
    std::thread::sleep(Duration::from_secs(30));
    let _ = holder.wait();
}

async fn changing_group_does_not_prevent_termination(cancelled: bool) {
    let fixture = DetachedFixture(temp_dir("changed-main-group"));
    std::fs::create_dir(&fixture.0).expect("create fixture");
    let mut plan = LaunchPlan::new(
        std::env::current_exe().expect("test executable"),
        fixture.0.clone(),
        capabilities([SandboxCapability::NoSandbox]),
        Duration::from_secs(5),
    );
    plan.args(["--exact", PROBE, "--nocapture"]);
    plan.environment.insert(MODE.into(), "parent".into());
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    plan.output_events = Some(sender);
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let ready = async move {
        let mut output = Vec::new();
        while let Some(event) = events.recv().await {
            output.extend(event.bytes);
            if output.windows(13).any(|bytes| bytes == b"group changed") {
                if cancelled {
                    cancel.cancel();
                }
                return;
            }
        }
        panic!("process exited before changing groups");
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(run(plan, cancellation), ready)
    })
    .await
    .expect("termination must not wait for the escaped main process");
    let output = result.expect("run changed process group");
    assert_eq!(
        output.termination,
        if cancelled {
            Termination::Cancelled
        } else {
            Termination::TimedOut
        }
    );
}

#[tokio::test]
async fn timeout_terminates_main_process_after_group_change() {
    changing_group_does_not_prevent_termination(false).await;
}

#[tokio::test]
async fn cancellation_terminates_main_process_after_group_change() {
    changing_group_does_not_prevent_termination(true).await;
}
