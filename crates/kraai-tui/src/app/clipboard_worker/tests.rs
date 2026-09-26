#![expect(
    clippy::panic_in_result_fn,
    reason = "clipboard worker tests propagate setup errors and assert isolation"
)]

use super::super::{RuntimeRequest, runtime_bridge::route_requests};
use super::*;
use crossbeam_channel::unbounded;

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
        Ok(ImageAttachment {
            id: "a".repeat(64),
            mime_type: "image/png".into(),
            width: 1,
            height: 1,
            byte_length: 1,
        })
    });
    let router =
        std::thread::spawn(move || route_requests(receiver, ordered, |id| worker.submit(id)));
    requests.send(RuntimeRequest::PasteImage { request_id: 1 })?;
    started.recv_timeout(Duration::from_secs(2))?;
    requests.send(RuntimeRequest::PasteImage { request_id: 2 })?;
    requests.send(RuntimeRequest::CancelStream {
        session_id: "session".into(),
    })?;
    assert!(matches!(
        ordinary.recv_timeout(Duration::from_secs(2))?,
        RuntimeRequest::CancelStream { .. }
    ));
    assert!(matches!(
        results.recv_timeout(Duration::from_secs(2))?,
        RuntimeResponse::PasteImage {
            request_id: 2,
            result: Err(_)
        }
    ));
    release.send(())?;
    assert!(matches!(
        results.recv_timeout(Duration::from_secs(2))?,
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
        Duration::from_secs(2),
        8,
    )
    .await?;
    assert_eq!(bytes, b"image");
    for script in [
        "while :; do printf 0123456789; done",
        "while :; do printf 0123456789 >&2; done",
    ] {
        let result = read_helper(shell(script), Duration::from_secs(2), 8).await;
        assert!(result.is_err_and(|error| error.to_string().contains("size limit")));
    }
    assert!(
        read_helper(shell("exit 1"), Duration::from_secs(2), 8)
            .await
            .is_err()
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn clipboard_helper_timeout_kills_and_reaps_the_process() -> color_eyre::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("helper-pid");
    let mut command = shell("echo $$ > \"$KRAAI_CLIPBOARD_TEST_PID\"; exec sleep 60");
    command.env("KRAAI_CLIPBOARD_TEST_PID", &path);
    let result = read_helper(command, Duration::from_millis(200), 8).await;
    assert!(result.is_err_and(|error| error.to_string().contains("timed out")));
    let pid = std::fs::read_to_string(path)?;
    let status = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()?;
    assert!(!status.success());
    Ok(())
}
