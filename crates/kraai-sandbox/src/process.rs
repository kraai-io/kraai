use std::process::Stdio;

#[cfg(unix)]
use nix::sys::signal::{Signal, killpg};
#[cfg(unix)]
use nix::unistd::Pid;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::config::{LaunchPlan, PreparedCommand};
use crate::error::SandboxError;
use crate::output::{ExecutionOutput, OutputEvent, OutputStream, Termination};
use crate::platform::prepare_command;

pub async fn run(
    plan: LaunchPlan,
    cancellation: CancellationToken,
) -> Result<ExecutionOutput, SandboxError> {
    let timeout = plan.timeout;
    let command = prepare_command(plan).await?;
    spawn_and_wait(command, timeout, cancellation).await
}

async fn spawn_and_wait(
    command: PreparedCommand,
    timeout: std::time::Duration,
    cancellation: CancellationToken,
) -> Result<ExecutionOutput, SandboxError> {
    let PreparedCommand {
        executable,
        args,
        cwd,
        environment,
        sandboxed,
        output_events,
        private_temp: _private_temp,
        #[cfg(target_os = "linux")]
        seccomp_filter,
    } = command;

    let stdin = {
        #[cfg(target_os = "linux")]
        {
            seccomp_filter.map_or_else(Stdio::null, Stdio::from)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Stdio::null()
        }
    };

    let mut process = Command::new(&executable);
    process
        .args(&args)
        .current_dir(&cwd)
        .env_clear()
        .envs(environment)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_process_tree(&mut process)?;
    let mut child = process.spawn().map_err(|error| SandboxError::Spawn {
        executable: executable.to_string_lossy().into_owned(),
        message: error.to_string(),
    })?;
    let process_group_id = child.id();
    let stdout = child.stdout.take().ok_or_else(|| {
        SandboxError::Wait(String::from(
            "spawned process did not provide a stdout pipe",
        ))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        SandboxError::Wait(String::from(
            "spawned process did not provide a stderr pipe",
        ))
    })?;
    let mut stdout_task = tokio::spawn(read_output(
        stdout,
        OutputStream::Stdout,
        output_events.clone(),
    ));
    let mut stderr_task = tokio::spawn(read_output(stderr, OutputStream::Stderr, output_events));

    let deadline = tokio::time::Instant::now() + timeout;
    let mut termination = tokio::select! {
        status = child.wait() => {
            let status = status.map_err(|error| SandboxError::Wait(error.to_string()))?;
            Termination::Exited { code: status.code() }
        }
        () = tokio::time::sleep_until(deadline) => {
            terminate_process_tree(&mut child, process_group_id).await?;
            Termination::TimedOut
        }
        () = cancellation.cancelled() => {
            terminate_process_tree(&mut child, process_group_id).await?;
            Termination::Cancelled
        }
    };

    let outputs = if matches!(termination, Termination::Exited { .. }) {
        tokio::select! {
            outputs = join_outputs(&mut stdout_task, &mut stderr_task) => outputs,
            () = tokio::time::sleep_until(deadline) => {
                terminate_process_tree(&mut child, process_group_id).await?;
                termination = Termination::TimedOut;
                join_outputs(&mut stdout_task, &mut stderr_task).await
            }
            () = cancellation.cancelled() => {
                terminate_process_tree(&mut child, process_group_id).await?;
                termination = Termination::Cancelled;
                join_outputs(&mut stdout_task, &mut stderr_task).await
            }
        }
    } else {
        join_outputs(&mut stdout_task, &mut stderr_task).await
    }?;
    let (stdout, stderr) = outputs;
    let exit_code = match termination {
        Termination::Exited { code } => code,
        Termination::TimedOut | Termination::Cancelled => None,
    };
    let sandbox_denied = sandboxed
        && is_likely_sandbox_denied(
            exit_code,
            &String::from_utf8_lossy(&stdout),
            &String::from_utf8_lossy(&stderr),
        );

    Ok(ExecutionOutput {
        termination,
        stdout,
        stderr,
        sandbox_denied,
    })
}

async fn read_output(
    mut reader: impl AsyncRead + Unpin,
    stream: OutputStream,
    events: Option<UnboundedSender<OutputEvent>>,
) -> std::io::Result<Vec<u8>> {
    let mut captured = Vec::new();
    let mut buffer = vec![0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(captured);
        }
        let bytes = buffer.iter().take(read).copied().collect::<Vec<_>>();
        captured.extend_from_slice(&bytes);
        if let Some(events) = &events {
            let _ = events.send(OutputEvent { stream, bytes });
        }
    }
}

async fn join_outputs(
    stdout: &mut tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
    stderr: &mut tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<(Vec<u8>, Vec<u8>), SandboxError> {
    let stdout = (&mut *stdout)
        .await
        .map_err(|error| SandboxError::Wait(error.to_string()))?
        .map_err(|error| SandboxError::Wait(error.to_string()))?;
    let stderr = (&mut *stderr)
        .await
        .map_err(|error| SandboxError::Wait(error.to_string()))?
        .map_err(|error| SandboxError::Wait(error.to_string()))?;
    Ok((stdout, stderr))
}

#[cfg(unix)]
fn configure_process_tree(process: &mut Command) -> Result<(), SandboxError> {
    use std::os::unix::process::CommandExt;

    process.as_std_mut().process_group(0);
    Ok(())
}

#[cfg(not(unix))]
fn configure_process_tree(_process: &mut Command) -> Result<(), SandboxError> {
    Ok(())
}

#[cfg(unix)]
async fn terminate_process_tree(
    child: &mut tokio::process::Child,
    process_group_id: Option<u32>,
) -> Result<(), SandboxError> {
    if let Some(process_group_id) = process_group_id {
        let process_group_id = i32::try_from(process_group_id)
            .map_err(|error| SandboxError::Wait(error.to_string()))?;
        if let Err(error) = killpg(Pid::from_raw(process_group_id), Signal::SIGKILL)
            && error != nix::errno::Errno::ESRCH
        {
            return Err(SandboxError::Wait(format!(
                "unable to terminate process group: {error}"
            )));
        }
    } else {
        child
            .kill()
            .await
            .map_err(|error| SandboxError::Wait(error.to_string()))?;
    }
    child
        .wait()
        .await
        .map_err(|error| SandboxError::Wait(error.to_string()))?;
    Ok(())
}

#[cfg(not(unix))]
async fn terminate_process_tree(
    child: &mut tokio::process::Child,
    _process_group_id: Option<u32>,
) -> Result<(), SandboxError> {
    child
        .kill()
        .await
        .map_err(|error| SandboxError::Wait(error.to_string()))?;
    child
        .wait()
        .await
        .map_err(|error| SandboxError::Wait(error.to_string()))?;
    Ok(())
}

pub(crate) fn is_likely_sandbox_denied(exit_code: Option<i32>, stdout: &str, stderr: &str) -> bool {
    if exit_code == Some(0) {
        return false;
    }

    const SANDBOX_DENIED_KEYWORDS: [&str; 8] = [
        "operation not permitted",
        "permission denied",
        "no permissions",
        "read-only file system",
        "seccomp",
        "sandbox",
        "landlock",
        "failed to write file",
    ];

    [stdout, stderr].into_iter().any(|section| {
        let lower = section.to_lowercase();
        SANDBOX_DENIED_KEYWORDS
            .iter()
            .any(|keyword| lower.contains(keyword))
    })
}
