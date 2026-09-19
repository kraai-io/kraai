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
    run_trusted_with_environment_inheritance(command, cwd, timeout, environment, true)
}

pub(crate) fn run_trusted_with_clean_environment(
    command: &[String],
    cwd: &Path,
    timeout: Duration,
    environment: &BTreeMap<String, String>,
) -> Result<CommandOutcome> {
    run_trusted_with_environment_inheritance(command, cwd, timeout, environment, false)
}

fn run_trusted_with_environment_inheritance(
    command: &[String],
    cwd: &Path,
    timeout: Duration,
    environment: &BTreeMap<String, String>,
    inherit_environment: bool,
) -> Result<CommandOutcome> {
    let Some(program) = command.first() else {
        bail!("command must not be empty");
    };
    with_temporary_log_root(|log_root| {
        let stdout_path = log_root.join("stdout");
        let stderr_path = log_root.join("stderr");
        let stdout = File::create(&stdout_path)?;
        let stderr = File::create(&stderr_path)?;
        let started = Instant::now();
        let mut process = Command::new(program);
        if !inherit_environment {
            process.env_clear();
        }
        process
            .args(command.get(1..).unwrap_or_default())
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        for (name, value) in environment {
            process.env(name, value);
        }
        let mut child = kraai_sandbox::spawn_command(&mut process)
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
        Ok(CommandOutcome {
            command: command.to_vec(),
            exit_code: status.code(),
            stdout: fs::read(stdout_path)?,
            stderr: fs::read(stderr_path)?,
            timed_out,
            output_limit_exceeded,
            duration,
        })
    })
}

#[cfg(all(test, unix))]
mod tests {
    use color_eyre::eyre::ensure;

    use super::*;

    #[test]
    fn failed_process_spawn_removes_owned_logs_and_preserves_the_error() -> Result<()> {
        let mut created = PathBuf::new();
        let result = with_temporary_log_root(|root| {
            created = root.to_path_buf();
            let mut process = Command::new(root.join("missing-program"));
            process
                .stdout(File::create(root.join("stdout"))?)
                .stderr(File::create(root.join("stderr"))?);
            let mut child = kraai_sandbox::spawn_command(&mut process)?;
            child.wait()?;
            Ok(())
        });
        let error = result
            .err()
            .ok_or_else(|| color_eyre::eyre::eyre!("missing command unexpectedly started"))?;
        ensure!(
            error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        );
        ensure!(!created.as_os_str().is_empty() && !created.exists());
        Ok(())
    }

    #[test]
    fn log_cleanup_preserves_primary_errors_and_reports_final_cleanup_errors() -> Result<()> {
        let primary: Result<()> = with_temporary_log_root(|root| {
            fs::remove_dir(root)?;
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "primary failure").into())
        });
        let error = primary
            .err()
            .ok_or_else(|| color_eyre::eyre::eyre!("primary failure was lost"))?;
        ensure!(error.to_string() == "primary failure");

        let cleanup = with_temporary_log_root(|root| {
            fs::remove_dir(root)?;
            Ok(())
        });
        let error = cleanup
            .err()
            .ok_or_else(|| color_eyre::eyre::eyre!("final cleanup failure was lost"))?;
        ensure!(
            error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        );
        Ok(())
    }

    #[test]
    fn clean_environment_does_not_inherit_controller_values() -> Result<()> {
        let env_program = std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|directory| directory.join("env"))
                    .find(|candidate| candidate.is_file())
            })
            .ok_or_else(|| color_eyre::eyre::eyre!("test requires env"))?;
        let command = vec![env_program.to_string_lossy().into_owned()];

        let outcome = run_trusted_with_clean_environment(
            &command,
            Path::new("/"),
            Duration::from_secs(5),
            &BTreeMap::new(),
        )?;

        ensure!(outcome.success(), "environment command failed");
        ensure!(
            outcome.stdout.is_empty(),
            "clean command inherited environment"
        );
        ensure!(
            outcome.stderr.is_empty(),
            "environment command wrote stderr"
        );
        Ok(())
    }
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |metadata| metadata.len())
}

struct TemporaryLogRoot {
    path: PathBuf,
    cleanup: bool,
}

impl TemporaryLogRoot {
    fn remove(mut self) -> std::io::Result<()> {
        self.cleanup = false;
        fs::remove_dir_all(&self.path)
    }
}

impl Drop for TemporaryLogRoot {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn with_temporary_log_root<T>(action: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    let path = std::env::temp_dir().join(format!("kraai-eval-command-{}", ulid::Ulid::generate()));
    fs::create_dir(&path)?;
    let root = TemporaryLogRoot {
        path,
        cleanup: true,
    };
    let result = action(&root.path)?;
    root.remove()?;
    Ok(result)
}
