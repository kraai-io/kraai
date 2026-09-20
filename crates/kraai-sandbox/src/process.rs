#[cfg(not(windows))]
use std::process::Stdio;

#[cfg(unix)]
#[path = "process_group.rs"]
mod process_group;
use tokio::io::{AsyncRead, AsyncReadExt};
#[cfg(not(windows))]
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::config::{LaunchPlan, PreparedCommand};
use crate::error::SandboxError;
use crate::output::{ExecutionOutput, OutputEvent, OutputStream, Termination};
use crate::platform::prepare_command;

pub async fn run(
    mut plan: LaunchPlan,
    cancellation: CancellationToken,
) -> Result<ExecutionOutput, SandboxError> {
    let timeout = plan.timeout;
    let process_spawned = plan.process_spawned.take();
    let execution_started = plan.execution_started.take();
    let mut command = prepare_command(plan).await?;
    let output = spawn_and_wait(
        &mut command,
        timeout,
        cancellation,
        process_spawned,
        execution_started,
    )
    .await;
    #[cfg(windows)]
    command.cleanup().await?;
    output
}

async fn spawn_and_wait(
    command: &mut PreparedCommand,
    timeout: std::time::Duration,
    cancellation: CancellationToken,
    process_spawned: Option<tokio::sync::oneshot::Sender<tokio::time::Instant>>,
    execution_started: Option<tokio::sync::oneshot::Receiver<tokio::time::Instant>>,
) -> Result<ExecutionOutput, SandboxError> {
    let sandboxed = command.sandboxed;
    let output_events = command.output_events.take();
    let mut child = spawn(command)?;
    if let Some(process_spawned) = process_spawned {
        let _ = process_spawned.send(tokio::time::Instant::now());
    }
    let _private_temp = &command.private_temp;
    #[cfg(unix)]
    let mut process_group = process_group::ProcessGroup::new(child.id())?;
    #[cfg(not(unix))]
    let mut process_group = ();
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
    let stop_output = CancellationToken::new();
    let _output_guard = stop_output.clone().drop_guard();
    let mut stdout_task = tokio::spawn(read_output(
        stdout,
        OutputStream::Stdout,
        output_events.clone(),
        stop_output.clone(),
    ));
    let mut stderr_task = tokio::spawn(read_output(
        stderr,
        OutputStream::Stderr,
        output_events,
        stop_output.clone(),
    ));

    let timeout_start = tokio::time::Instant::now();
    let deadline = async move {
        let started = match execution_started {
            Some(started) => match started.await {
                Ok(started) => started,
                Err(_closed) => return std::future::pending::<()>().await,
            },
            None => timeout_start,
        };
        tokio::time::sleep_until(started + timeout).await;
    };
    tokio::pin!(deadline);
    let mut termination = tokio::select! {
        status = child.wait() => {
            let status = status.map_err(|error| SandboxError::Wait(error.to_string()))?;
            #[cfg(unix)]
            process_group.kill()?;
            Termination::Exited { code: status.code() }
        }
        () = &mut deadline => {
            terminate_process_tree(&mut child, &mut process_group).await?;
            Termination::TimedOut
        }
        () = cancellation.cancelled() => {
            terminate_process_tree(&mut child, &mut process_group).await?;
            Termination::Cancelled
        }
    };

    let outputs = join_outputs(&mut stdout_task, &mut stderr_task);
    tokio::pin!(outputs);
    let outputs = if matches!(termination, Termination::Exited { .. }) {
        tokio::select! {
            outputs = &mut outputs => outputs,
            () = &mut deadline => {
                terminate_process_tree(&mut child, &mut process_group).await?;
                termination = Termination::TimedOut;
                finish_outputs(&mut outputs, &stop_output).await
            }
            () = cancellation.cancelled() => {
                terminate_process_tree(&mut child, &mut process_group).await?;
                termination = Termination::Cancelled;
                finish_outputs(&mut outputs, &stop_output).await
            }
        }
    } else {
        finish_outputs(&mut outputs, &stop_output).await
    }?;
    let (stdout, stderr) = outputs;
    let exit_code = match termination {
        Termination::Exited { code } => code,
        Termination::TimedOut | Termination::Cancelled => None,
    };
    let sandbox_denied = sandboxed && is_likely_sandbox_denied(exit_code, &stdout, &stderr);

    Ok(ExecutionOutput {
        termination,
        stdout,
        stderr,
        sandbox_denied,
    })
}

#[cfg(windows)]
use crate::platform::windows::process::spawn;

#[cfg(not(windows))]
fn spawn(command: &mut PreparedCommand) -> Result<tokio::process::Child, SandboxError> {
    let stdin = {
        #[cfg(target_os = "linux")]
        {
            command
                .seccomp_filter
                .take()
                .map_or_else(Stdio::null, Stdio::from)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Stdio::null()
        }
    };
    let mut process = Command::new(&command.executable);
    process
        .args(&command.args)
        .current_dir(&command.cwd)
        .env_clear()
        .envs(&command.environment)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_process_tree(&mut process)?;
    process.spawn().map_err(|error| SandboxError::Spawn {
        executable: command.executable.to_string_lossy().into_owned(),
        message: error.to_string(),
    })
}

#[cfg(windows)]
async fn terminate_process_tree(
    child: &mut crate::platform::windows::process::Child,
    _process_group: &mut (),
) -> Result<(), SandboxError> {
    child.terminate().await
}

async fn read_output(
    mut reader: impl AsyncRead + Unpin,
    stream: OutputStream,
    events: Option<UnboundedSender<OutputEvent>>,
    cancellation: CancellationToken,
) -> std::io::Result<Vec<u8>> {
    let mut captured = Vec::new();
    let mut buffer = vec![0_u8; 16 * 1024];
    loop {
        let read = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Ok(captured),
            read = reader.read(&mut buffer) => read?,
        };
        if read == 0 {
            return Ok(captured);
        }
        let bytes = buffer.get(..read).unwrap_or(&buffer);
        captured.extend_from_slice(bytes);
        if let Some(events) = &events {
            let _ = events.send(OutputEvent {
                stream,
                bytes: bytes.to_vec(),
            });
        }
    }
}

async fn finish_outputs(
    outputs: impl Future<Output = Result<(Vec<u8>, Vec<u8>), SandboxError>>,
    stop_output: &CancellationToken,
) -> Result<(Vec<u8>, Vec<u8>), SandboxError> {
    tokio::pin!(outputs);
    tokio::select! {
        output = &mut outputs => output,
        () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
            stop_output.cancel();
            outputs.await
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

#[cfg(not(any(unix, windows)))]
fn configure_process_tree(_process: &mut Command) -> Result<(), SandboxError> {
    Ok(())
}

#[cfg(unix)]
async fn terminate_process_tree(
    child: &mut tokio::process::Child,
    process_group: &mut process_group::ProcessGroup,
) -> Result<(), SandboxError> {
    process_group.kill()?;
    child
        .kill()
        .await
        .map_err(|error| SandboxError::Wait(error.to_string()))?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
async fn terminate_process_tree(
    child: &mut tokio::process::Child,
    _process_group: &mut (),
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

pub(crate) fn is_likely_sandbox_denied(
    exit_code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> bool {
    if exit_code == Some(0) {
        return false;
    }

    const SANDBOX_DENIED_KEYWORDS: [&str; 9] = [
        "operation not permitted",
        "access is denied",
        "permission denied",
        "no permissions",
        "read-only file system",
        "seccomp",
        "sandbox",
        "landlock",
        "failed to write file",
    ];

    [stdout, stderr].into_iter().any(|section| {
        let lower = String::from_utf8_lossy(section).to_lowercase();
        SANDBOX_DENIED_KEYWORDS
            .iter()
            .any(|keyword| lower.contains(keyword))
    })
}
