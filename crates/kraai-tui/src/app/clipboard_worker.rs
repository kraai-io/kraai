use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use crossbeam_channel::{Sender, bounded};
use kraai_runtime::{RuntimeError, RuntimeHandle, RuntimeResult};
use kraai_types::{ImageAttachment, image::MAX_IMAGE_BYTES};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::RuntimeResponse;

pub(super) struct ClipboardWorker {
    requests: Sender<u64>,
    responses: Sender<RuntimeResponse>,
    busy: Arc<AtomicBool>,
}

impl ClipboardWorker {
    pub(super) fn spawn(runtime: RuntimeHandle, responses: Sender<RuntimeResponse>) -> Self {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                RuntimeError::unavailable(format!("Cannot start clipboard worker: {error}"))
            });
        Self::spawn_job(responses, move || match &executor {
            Ok(executor) => executor.block_on(async {
                let executable = std::env::current_exe().map_err(|error| {
                    RuntimeError::unavailable(format!("Cannot locate clipboard helper: {error}"))
                })?;
                let mut command = tokio::process::Command::new(executable);
                command.arg(crate::clipboard_image::HELPER_ARG);
                let bytes = read_helper(command, Duration::from_secs(15), MAX_IMAGE_BYTES).await?;
                runtime.import_image(bytes).await
            }),
            Err(error) => Err(error.clone()),
        })
    }

    fn spawn_job(
        responses: Sender<RuntimeResponse>,
        mut job: impl FnMut() -> RuntimeResult<ImageAttachment> + Send + 'static,
    ) -> Self {
        let (requests, receiver) = bounded(1);
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = busy.clone();
        let worker_responses = responses.clone();
        std::thread::spawn(move || {
            while let Ok(request_id) = receiver.recv() {
                let result = job();
                worker_busy.store(false, Ordering::Release);
                if worker_responses
                    .send(RuntimeResponse::PasteImage { request_id, result })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            requests,
            responses,
            busy,
        }
    }

    pub(super) fn submit(&self, request_id: u64) {
        let error = if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            Some("A clipboard image import is already running")
        } else if self.requests.try_send(request_id).is_err() {
            self.busy.store(false, Ordering::Release);
            Some("Clipboard worker is unavailable")
        } else {
            None
        };
        if let Some(error) = error {
            let _ = self.responses.send(RuntimeResponse::PasteImage {
                request_id,
                result: Err(RuntimeError::unavailable(error)),
            });
        }
    }
}

async fn read_helper(
    mut command: tokio::process::Command,
    timeout: Duration,
    max_bytes: usize,
) -> RuntimeResult<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = kraai_sandbox::spawn_tokio_command(&mut command).map_err(|error| {
        RuntimeError::unavailable(format!("Cannot launch clipboard helper: {error}"))
    })?;
    let outcome = tokio::time::timeout(timeout, async {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("Clipboard helper stdout is missing"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("Clipboard helper stderr is missing"))?;
        let (status, bytes, diagnostic) = tokio::try_join!(
            child.wait(),
            read_bounded(stdout, max_bytes),
            read_bounded(stderr, 4096)
        )?;
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "Clipboard helper failed: {}",
                String::from_utf8_lossy(&diagnostic).trim()
            )));
        }
        if bytes.is_empty() {
            return Err(std::io::Error::other("Clipboard helper returned no image"));
        }
        Ok(bytes)
    })
    .await;
    let error = match outcome {
        Ok(Ok(bytes)) => return Ok(bytes),
        Ok(Err(error)) => error.to_string(),
        Err(_) => String::from("Clipboard image read timed out"),
    };
    let _ = child.start_kill();
    let _ = child.wait().await;
    Err(RuntimeError::unavailable(error))
}

async fn read_bounded(reader: impl AsyncRead + Unpin, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(std::io::Error::other(
            "Clipboard helper output exceeds the size limit",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
