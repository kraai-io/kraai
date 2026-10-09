use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, bail, ensure};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, raise};
use nix::sys::wait::{WaitPidFlag, waitpid};
use nix::unistd::{Pid, getpgid, getpid};

use super::sample;
use crate::process::{self, Interrupt, IsolatedWorkspace};
use crate::test_support::{fixture_command, run_fixture};

const FIXTURE_DIRECTORY: &str = "KRAAI_PERF_FIXTURE_DIRECTORY";

#[test]
fn interrupting_worker_allows_child_cleanup_and_reaps_worker() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let interrupted = Arc::new(AtomicBool::new(false));
    let worker = spawn_fixture(
        "worker_with_child_fixture",
        directory.path(),
        interrupted.clone(),
    )?;
    wait_for_file(&directory.path().join("worker-ready"))?;
    let worker_pid =
        Pid::from_raw(fs::read_to_string(directory.path().join("worker-pid"))?.parse()?);
    let process_group = getpgid(Some(worker_pid))?;
    ensure!(
        process_group == worker_pid,
        "worker did not start its own process group"
    );
    let descendant_pid =
        Pid::from_raw(fs::read_to_string(directory.path().join("child-pid"))?.parse()?);

    interrupted.store(true, Ordering::Relaxed);
    let result = worker
        .join()
        .map_err(|_panic_payload| color_eyre::eyre::eyre!("worker supervisor panicked"))?;
    ensure!(result.is_err(), "interrupted worker succeeded");

    ensure!(
        directory.path().join("cleaned-up").exists(),
        "worker did not finish child cleanup"
    );
    let wait_result = waitpid(worker_pid, Some(WaitPidFlag::WNOHANG));
    ensure!(wait_result == Err(Errno::ECHILD), "worker was not reaped");
    let descendant_status = kill(descendant_pid, None);
    ensure!(
        descendant_status == Err(Errno::ESRCH),
        "worker descendant is still present"
    );
    Ok(())
}

#[test]
fn wrapper_exit_cleans_up_remaining_process_group() -> Result<()> {
    run_fixture(module_path!(), "wrapper_cleanup_fixture")
}

#[test]
#[ignore = "invoked in a separate process to isolate child reaping"]
fn wrapper_cleanup_fixture() -> Result<()> {
    #[cfg(target_os = "linux")]
    nix::sys::prctl::set_child_subreaper(true)?;
    let directory = tempfile::tempdir()?;
    let mut command = fixture_command(module_path!(), "early_exit_wrapper_fixture")?;
    command.env(FIXTURE_DIRECTORY, directory.path());
    process::run(
        &mut command,
        Duration::from_secs(10),
        &AtomicBool::new(false),
        &directory.path().join("stdout"),
        &directory.path().join("stderr"),
    )?;
    ensure!(
        directory.path().join("orphan-terminated").exists(),
        "wrapper exit left its descendant running"
    );
    #[cfg(target_os = "linux")]
    {
        let descendant_pid =
            Pid::from_raw(fs::read_to_string(directory.path().join("child-pid"))?.parse()?);
        let status = waitpid(descendant_pid, None)?;
        ensure!(
            matches!(status, nix::sys::wait::WaitStatus::Exited(_, 0)),
            "orphan did not exit after graceful termination"
        );
    }
    Ok(())
}

#[test]
#[ignore = "invoked by the wrapper cleanup fixture"]
fn early_exit_wrapper_fixture() -> Result<()> {
    let directory = fixture_directory()?;
    let mut command = fixture_command(module_path!(), "orphan_descendant_fixture")?;
    command.env(FIXTURE_DIRECTORY, &directory);
    let mut child = command.spawn()?;
    let _reaper = std::thread::spawn(move || child.wait());
    wait_for_file(&directory.join("child-ready"))?;
    Ok(())
}

#[test]
#[ignore = "invoked as a descendant left behind by the wrapper fixture"]
fn orphan_descendant_fixture() -> Result<()> {
    let directory = fixture_directory()?;
    let terminated = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, terminated.clone())?;
    fs::write(directory.join("child-pid"), getpid().to_string())?;
    fs::write(directory.join("child-ready"), b"ready")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !terminated.load(Ordering::Relaxed) {
        ensure!(
            Instant::now() < deadline,
            "orphaned descendant was not terminated"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    fs::write(directory.join("orphan-terminated"), b"terminated")?;
    Ok(())
}

#[test]
fn isolated_workspace_sets_environment_and_cleans_up() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let workspace = IsolatedWorkspace::new(directory.path())?;
    let mut command = Command::new("unused");
    command.env("REMOVE_ME", "inherited");
    workspace.configure(&mut command);
    let current_dir = command
        .get_current_dir()
        .ok_or_else(|| color_eyre::eyre::eyre!("isolated command has no directory"))?
        .to_path_buf();
    ensure!(current_dir.starts_with(fs::canonicalize(directory.path())?));
    let environment: std::collections::BTreeMap<_, _> = command.get_envs().collect();
    let home = current_dir.join("home");
    ensure!(home.exists(), "isolated home was not created");
    ensure!(
        environment.get(std::ffi::OsStr::new("HOME")) == Some(&Some(home.as_os_str())),
        "isolated home was not passed to the worker"
    );
    ensure!(
        !environment.contains_key(std::ffi::OsStr::new("REMOVE_ME")),
        "command retained a previous environment override"
    );
    drop(workspace);
    ensure!(!current_dir.exists(), "isolated workspace was not removed");
    Ok(())
}

#[test]
fn interrupted_process_does_not_create_output_files() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let stdout = directory.path().join("stdout");
    let stderr = directory.path().join("stderr");
    let mut command = Command::new("unused");
    let result = process::run(
        &mut command,
        Duration::from_secs(1),
        &AtomicBool::new(true),
        &stdout,
        &stderr,
    );
    ensure!(result.is_err(), "interrupted command succeeded");
    ensure!(!stdout.exists(), "interrupted command created stdout");
    ensure!(!stderr.exists(), "interrupted command created stderr");
    Ok(())
}

#[test]
fn interrupted_sample_does_not_create_work_directory() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let missing = directory.path().join("missing");
    let result = sample("unused", &missing, 1, &AtomicBool::new(true));
    let interrupted = result.is_err_and(|error| error.to_string() == "performance run interrupted");
    ensure!(interrupted, "sample did not report interruption");
    ensure!(!missing.exists(), "interrupted sample created a directory");
    Ok(())
}

#[test]
fn interrupt_flags_are_new_for_each_run() -> Result<()> {
    run_fixture(module_path!(), "interrupt_fixture")
}

#[test]
#[ignore = "invoked in a separate process to isolate signal handling"]
fn interrupt_fixture() -> Result<()> {
    let first = Interrupt::new()?;
    raise(Signal::SIGINT)?;
    ensure!(
        first.flag.load(Ordering::Relaxed),
        "SIGINT did not set interruption flag"
    );
    drop(first);

    let second = Interrupt::new()?;
    ensure!(
        !second.flag.load(Ordering::Relaxed),
        "new run retained interruption flag"
    );
    raise(Signal::SIGTERM)?;
    ensure!(
        second.flag.load(Ordering::Relaxed),
        "SIGTERM did not set interruption flag"
    );
    drop(second);
    Ok(())
}

#[test]
#[ignore = "invoked by the worker lifecycle test"]
fn worker_with_child_fixture() -> Result<()> {
    let directory = fixture_directory()?;
    let terminated = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, terminated.clone())?;
    let child = spawn_fixture("descendant_fixture", &directory, terminated.clone())?;
    wait_for_file(&directory.join("child-ready"))?;
    fs::write(directory.join("worker-pid"), getpid().to_string())?;
    fs::write(directory.join("worker-ready"), b"ready")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !terminated.load(Ordering::Relaxed) {
        ensure!(
            Instant::now() < deadline,
            "worker fixture was not terminated"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let result = child
        .join()
        .map_err(|_panic_payload| color_eyre::eyre::eyre!("child supervisor panicked"))?;
    ensure!(result.is_err(), "terminated child succeeded");
    fs::write(directory.join("cleaned-up"), b"reaped")?;
    Ok(())
}

#[test]
#[ignore = "invoked as a child by the worker lifecycle fixture"]
fn descendant_fixture() -> Result<()> {
    let directory = fixture_directory()?;
    fs::write(directory.join("child-pid"), getpid().to_string())?;
    fs::write(directory.join("child-ready"), b"ready")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    bail!("descendant fixture was not terminated")
}

fn fixture_directory() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os(FIXTURE_DIRECTORY)
            .ok_or_else(|| color_eyre::eyre::eyre!("worker fixture directory missing"))?,
    ))
}

fn spawn_fixture(
    name: &str,
    directory: &Path,
    interrupted: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<Result<()>>> {
    let mut command = fixture_command(module_path!(), name)?;
    command.env(FIXTURE_DIRECTORY, directory);
    let stdout = directory.join(format!("{name}.stdout"));
    let stderr = directory.join(format!("{name}.stderr"));
    Ok(std::thread::spawn(move || {
        process::run(
            &mut command,
            Duration::from_secs(10),
            &interrupted,
            &stdout,
            &stderr,
        )
    }))
}

fn wait_for_file(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        ensure!(
            Instant::now() < deadline,
            "fixture did not create {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
