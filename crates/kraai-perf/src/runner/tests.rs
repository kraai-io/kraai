use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use color_eyre::eyre::Result;
use kraai_test_support::HANG_TIMEOUT;
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, raise};
use nix::sys::wait::{WaitPidFlag, waitpid};
use nix::unistd::{Pid, getpgid, getpid};

use super::sample;
use crate::process::{self, Interrupt, IsolatedWorkspace};
use crate::test_support::{fixture_command, run_fixture};

const FIXTURE_DIRECTORY: &str = "KRAAI_PERF_FIXTURE_DIRECTORY";

#[test]
fn interrupting_worker_allows_child_cleanup_and_reaps_worker() {
    let directory = tempfile::tempdir().unwrap();
    let interrupted = Arc::new(AtomicBool::new(false));
    let worker = spawn_fixture(
        "worker_with_child_fixture",
        directory.path(),
        interrupted.clone(),
    );
    wait_for_file(&directory.path().join("worker-ready"));
    let worker_pid = Pid::from_raw(
        fs::read_to_string(directory.path().join("worker-pid"))
            .unwrap()
            .parse()
            .unwrap(),
    );
    let process_group = getpgid(Some(worker_pid)).unwrap();
    assert!(
        process_group == worker_pid,
        "worker did not start its own process group"
    );
    let descendant_pid = Pid::from_raw(
        fs::read_to_string(directory.path().join("child-pid"))
            .unwrap()
            .parse()
            .unwrap(),
    );

    interrupted.store(true, Ordering::Relaxed);
    let result = worker.join().expect("worker supervisor panicked");
    assert!(result.is_err(), "interrupted worker succeeded");

    assert!(
        directory.path().join("cleaned-up").exists(),
        "worker did not finish child cleanup"
    );
    let wait_result = waitpid(worker_pid, Some(WaitPidFlag::WNOHANG));
    assert!(wait_result == Err(Errno::ECHILD), "worker was not reaped");
    let descendant_status = kill(descendant_pid, None);
    assert!(
        descendant_status == Err(Errno::ESRCH),
        "worker descendant is still present"
    );
}

#[test]
fn wrapper_exit_cleans_up_remaining_process_group() {
    run_fixture(module_path!(), "wrapper_cleanup_fixture");
}

#[test]
#[ignore = "invoked in a separate process to isolate child reaping"]
fn wrapper_cleanup_fixture() {
    #[cfg(target_os = "linux")]
    nix::sys::prctl::set_child_subreaper(true).unwrap();
    let directory = tempfile::tempdir().unwrap();
    #[cfg(target_os = "linux")]
    let orphan = {
        let directory = directory.path().to_path_buf();
        std::thread::spawn(move || reap_orphan(&directory))
    };
    let mut command = fixture_command(module_path!(), "early_exit_wrapper_fixture");
    command.env(FIXTURE_DIRECTORY, directory.path());
    process::run(
        &mut command,
        HANG_TIMEOUT,
        HANG_TIMEOUT,
        &AtomicBool::new(false),
        &directory.path().join("stdout"),
        &directory.path().join("stderr"),
    )
    .unwrap();
    assert!(
        directory.path().join("orphan-terminated").exists(),
        "wrapper exit left its descendant running"
    );
    #[cfg(target_os = "linux")]
    {
        let status = orphan.join().expect("orphan reaper panicked");
        assert!(
            matches!(status, nix::sys::wait::WaitStatus::Exited(_, 0)),
            "orphan did not exit after graceful termination"
        );
    }
}

#[cfg(target_os = "linux")]
fn reap_orphan(directory: &Path) -> nix::sys::wait::WaitStatus {
    wait_for_file(&directory.join("child-ready"));
    let pid = Pid::from_raw(
        fs::read_to_string(directory.join("child-pid"))
            .unwrap()
            .parse()
            .unwrap(),
    );
    let deadline = Instant::now() + HANG_TIMEOUT;
    loop {
        match waitpid(pid, None) {
            Err(Errno::ECHILD | Errno::EINTR) => {
                assert!(
                    Instant::now() < deadline,
                    "orphan was not reparented to the subreaper"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            result => return result.unwrap(),
        }
    }
}

#[test]
#[ignore = "invoked by the wrapper cleanup fixture"]
fn early_exit_wrapper_fixture() {
    let directory = fixture_directory();
    let mut command = fixture_command(module_path!(), "orphan_descendant_fixture");
    command.env(FIXTURE_DIRECTORY, &directory);
    let mut child = command.spawn().unwrap();
    let _reaper = std::thread::spawn(move || child.wait());
    wait_for_file(&directory.join("child-ready"));
}

#[test]
#[ignore = "invoked as a descendant left behind by the wrapper fixture"]
fn orphan_descendant_fixture() {
    let directory = fixture_directory();
    let terminated = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, terminated.clone()).unwrap();
    fs::write(directory.join("child-pid"), getpid().to_string()).unwrap();
    fs::write(directory.join("child-ready"), b"ready").unwrap();
    let deadline = Instant::now() + HANG_TIMEOUT;
    while !terminated.load(Ordering::Relaxed) {
        assert!(
            Instant::now() < deadline,
            "orphaned descendant was not terminated"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    fs::write(directory.join("orphan-terminated"), b"terminated").unwrap();
}

#[test]
fn isolated_workspace_sets_environment_and_cleans_up() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = IsolatedWorkspace::new(directory.path()).unwrap();
    let mut command = Command::new("unused");
    command.env("REMOVE_ME", "inherited");
    workspace.configure(&mut command);
    let current_dir = command
        .get_current_dir()
        .expect("isolated command has no directory")
        .to_path_buf();
    assert!(current_dir.starts_with(fs::canonicalize(directory.path()).unwrap()));
    let environment: std::collections::BTreeMap<_, _> = command.get_envs().collect();
    let home = current_dir.join("home");
    assert!(home.exists(), "isolated home was not created");
    assert!(
        environment.get(std::ffi::OsStr::new("HOME")) == Some(&Some(home.as_os_str())),
        "isolated home was not passed to the worker"
    );
    assert!(
        !environment.contains_key(std::ffi::OsStr::new("REMOVE_ME")),
        "command retained a previous environment override"
    );
    drop(workspace);
    assert!(!current_dir.exists(), "isolated workspace was not removed");
}

#[test]
fn interrupted_process_does_not_create_output_files() {
    let directory = tempfile::tempdir().unwrap();
    let stdout = directory.path().join("stdout");
    let stderr = directory.path().join("stderr");
    let mut command = Command::new("unused");
    let result = process::run(
        &mut command,
        Duration::from_secs(1),
        process::TERMINATION_GRACE,
        &AtomicBool::new(true),
        &stdout,
        &stderr,
    );
    assert!(result.is_err(), "interrupted command succeeded");
    assert!(!stdout.exists(), "interrupted command created stdout");
    assert!(!stderr.exists(), "interrupted command created stderr");
}

#[test]
fn interrupted_sample_does_not_create_work_directory() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing");
    let result = sample("unused", &missing, 1, &AtomicBool::new(true));
    let interrupted = result.is_err_and(|error| error.to_string() == "performance run interrupted");
    assert!(interrupted, "sample did not report interruption");
    assert!(!missing.exists(), "interrupted sample created a directory");
}

#[test]
fn interrupt_flags_are_new_for_each_run() {
    run_fixture(module_path!(), "interrupt_fixture");
}

#[test]
#[ignore = "invoked in a separate process to isolate signal handling"]
fn interrupt_fixture() {
    let first = Interrupt::new().unwrap();
    raise(Signal::SIGINT).unwrap();
    assert!(
        first.flag.load(Ordering::Relaxed),
        "SIGINT did not set interruption flag"
    );
    drop(first);

    let second = Interrupt::new().unwrap();
    assert!(
        !second.flag.load(Ordering::Relaxed),
        "new run retained interruption flag"
    );
    raise(Signal::SIGTERM).unwrap();
    assert!(
        second.flag.load(Ordering::Relaxed),
        "SIGTERM did not set interruption flag"
    );
    drop(second);
}

#[test]
#[ignore = "invoked by the worker lifecycle test"]
fn worker_with_child_fixture() {
    let directory = fixture_directory();
    let terminated = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, terminated.clone()).unwrap();
    let child = spawn_fixture("descendant_fixture", &directory, terminated.clone());
    wait_for_file(&directory.join("child-ready"));
    fs::write(directory.join("worker-pid"), getpid().to_string()).unwrap();
    fs::write(directory.join("worker-ready"), b"ready").unwrap();
    let deadline = Instant::now() + HANG_TIMEOUT;
    while !terminated.load(Ordering::Relaxed) {
        assert!(
            Instant::now() < deadline,
            "worker fixture was not terminated"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let result = child.join().expect("child supervisor panicked");
    assert!(result.is_err(), "terminated child succeeded");
    fs::write(directory.join("cleaned-up"), b"reaped").unwrap();
}

#[test]
#[ignore = "invoked as a child by the worker lifecycle fixture"]
fn descendant_fixture() {
    let directory = fixture_directory();
    fs::write(directory.join("child-pid"), getpid().to_string()).unwrap();
    fs::write(directory.join("child-ready"), b"ready").unwrap();
    let deadline = Instant::now() + HANG_TIMEOUT;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("descendant fixture was not terminated")
}

fn fixture_directory() -> PathBuf {
    PathBuf::from(std::env::var_os(FIXTURE_DIRECTORY).expect("worker fixture directory missing"))
}

fn spawn_fixture(
    name: &str,
    directory: &Path,
    interrupted: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Result<()>> {
    let mut command = fixture_command(module_path!(), name);
    command.env(FIXTURE_DIRECTORY, directory);
    let stdout = directory.join(format!("{name}.stdout"));
    let stderr = directory.join(format!("{name}.stderr"));
    std::thread::spawn(move || {
        process::run(
            &mut command,
            HANG_TIMEOUT,
            HANG_TIMEOUT,
            &interrupted,
            &stdout,
            &stderr,
        )
    })
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + HANG_TIMEOUT;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "fixture did not create {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
