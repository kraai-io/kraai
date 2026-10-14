#![expect(
    clippy::panic_in_result_fn,
    reason = "clipboard worker tests propagate setup errors and assert isolation"
)]

use super::super::{RuntimeRequest, runtime_bridge::route_requests};
use super::*;
use crossbeam_channel::unbounded;
use kraai_types::SessionId;

#[test]
fn clipboard_work_does_not_block_ordered_requests_and_rejects_queued_pastes()
-> color_eyre::Result<()> {
    let (requests, receiver) = unbounded();
    let (ordered, ordinary) = unbounded();
    let (responses, results) = unbounded();
    let (entered, started) = bounded(1);
    let (release, released) = bounded(1);
    let worker = ClipboardWorker::spawn_job(responses, move || {
        entered
            .send(())
            .map_err(|error| RuntimeError::unavailable(error.to_string()))?;
        released
            .recv()
            .map_err(|error| RuntimeError::unavailable(error.to_string()))?;
        Ok(Some(ImageAttachment {
            id: "a".repeat(64),
            mime_type: "image/png".into(),
            width: 1,
            height: 1,
            byte_length: 1,
        }))
    });
    let router =
        std::thread::spawn(move || route_requests(receiver, ordered, |id| worker.submit(id)));
    requests.send(RuntimeRequest::PasteImage { request_id: 1 })?;
    started.recv_timeout(kraai_test_support::HANG_TIMEOUT)?;
    requests.send(RuntimeRequest::PasteImage { request_id: 2 })?;
    requests.send(RuntimeRequest::CancelStream {
        session_id: SessionId::new("session"),
    })?;
    assert!(matches!(
        ordinary.recv_timeout(kraai_test_support::HANG_TIMEOUT)?,
        RuntimeRequest::CancelStream { .. }
    ));
    assert!(matches!(
        results.recv_timeout(kraai_test_support::HANG_TIMEOUT)?,
        RuntimeResponse::PasteImage {
            request_id: 2,
            result: Err(_)
        }
    ));
    release.send(())?;
    assert!(matches!(
        results.recv_timeout(kraai_test_support::HANG_TIMEOUT)?,
        RuntimeResponse::PasteImage {
            request_id: 1,
            result: Ok(_)
        }
    ));
    drop(requests);
    assert!(router.join().is_ok());
    Ok(())
}

#[cfg(unix)]
fn shell(script: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("sh");
    command.args(["-c", script]);
    command
}

#[cfg(unix)]
#[tokio::test]
async fn clipboard_helper_caps_both_output_streams_and_returns_complete_bytes()
-> color_eyre::Result<()> {
    let bytes = read_helper(
        shell("printf image; printf diagnostic >&2"),
        kraai_test_support::HANG_TIMEOUT,
        8,
    )
    .await?;
    assert_eq!(bytes.as_deref(), Some(b"image".as_slice()));
    for script in [
        "while :; do printf 0123456789; done",
        "while :; do printf 0123456789 >&2; done",
    ] {
        let result = read_helper(shell(script), kraai_test_support::HANG_TIMEOUT, 8).await;
        assert!(result.is_err_and(|error| error.to_string().contains("size limit")));
    }
    assert!(
        read_helper(shell("exit 1"), kraai_test_support::HANG_TIMEOUT, 8)
            .await
            .is_err()
    );
    assert!(
        read_helper(shell("exit 0"), kraai_test_support::HANG_TIMEOUT, 8)
            .await
            .is_err()
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn clipboard_helper_reports_missing_image_without_an_error() -> color_eyre::Result<()> {
    let command = shell(&format!(
        "exit {}",
        crate::clipboard_image::NO_IMAGE_EXIT_CODE
    ));
    assert!(
        read_helper(command, kraai_test_support::HANG_TIMEOUT, 8)
            .await?
            .is_none()
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn clipboard_helper_timeout_kills_and_reaps_the_process() -> color_eyre::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("helper-pid");
    // Under load the helper can time out before the shell records its pid; retry with a longer timeout.
    let mut timeout = Duration::from_millis(200);
    let pid = loop {
        let mut command = shell("echo $$ > \"$KRAAI_CLIPBOARD_TEST_PID\"; exec sleep 60");
        command.env("KRAAI_CLIPBOARD_TEST_PID", &path);
        let result = read_helper(command, timeout, 8).await;
        assert!(result.is_err_and(|error| error.to_string().contains("timed out")));
        let pid = std::fs::read_to_string(&path).unwrap_or_default();
        if !pid.trim().is_empty() {
            break pid;
        }
        assert!(
            timeout < kraai_test_support::HANG_TIMEOUT,
            "helper never recorded its pid"
        );
        timeout *= 2;
    };
    let status = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()?;
    assert!(!status.success());
    Ok(())
}
