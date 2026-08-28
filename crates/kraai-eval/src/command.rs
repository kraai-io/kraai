use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Context, Result, bail};

const MAX_CAPTURED_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct CommandOutcome {
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub output_limit_exceeded: bool,
    pub duration: Duration,
}

impl CommandOutcome {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out && !self.output_limit_exceeded
    }
}

pub(crate) fn run_trusted(
    command: &[String],
    cwd: &Path,
    timeout: Duration,
) -> Result<CommandOutcome> {
    run_trusted_with_environment(command, cwd, timeout, &BTreeMap::new())
}

pub(crate) fn run_trusted_with_environment(
    command: &[String],
    cwd: &Path,
    timeout: Duration,
    environment: &BTreeMap<String, String>,
) -> Result<CommandOutcome> {
    let Some(program) = command.first() else {
        bail!("command must not be empty");
    };
    let log_root = temporary_log_root()?;
    let stdout_path = log_root.join("stdout");
    let stderr_path = log_root.join("stderr");
    let stdout = File::create(&stdout_path)?;
    let stderr = File::create(&stderr_path)?;
    let started = Instant::now();
    let mut process = Command::new(program);
    process
        .args(command.get(1..).unwrap_or_default())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (name, value) in environment {
        process.env(name, value);
    }
    let mut child = process
        .spawn()
        .wrap_err_with(|| format!("spawn trusted command {program}"))?;
    let (status, timed_out, output_limit_exceeded) = loop {
        if let Some(status) = child.try_wait()? {
            break (status, false, false);
        }
        if started.elapsed() >= timeout {
            child
                .kill()
                .wrap_err_with(|| format!("kill timed out command {program}"))?;
            break (child.wait()?, true, false);
        }
        let captured_bytes = file_size(&stdout_path).saturating_add(file_size(&stderr_path));
        if captured_bytes > MAX_CAPTURED_OUTPUT_BYTES {
            child
                .kill()
                .wrap_err_with(|| format!("kill output-limited command {program}"))?;
            break (child.wait()?, false, true);
        }
        thread::sleep(Duration::from_millis(10));
    };
    let duration = started.elapsed();
    let outcome = CommandOutcome {
        command: command.to_vec(),
        exit_code: status.code(),
        stdout: fs::read(stdout_path)?,
        stderr: fs::read(stderr_path)?,
        timed_out,
        output_limit_exceeded,
        duration,
    };
    fs::remove_dir_all(log_root)?;
    Ok(outcome)
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn temporary_log_root() -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("kraai-eval-command-{}", ulid::Ulid::generate()));
    fs::create_dir(&path)?;
    Ok(path)
}
