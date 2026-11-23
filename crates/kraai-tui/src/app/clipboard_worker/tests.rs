use super::super::{RuntimeRequest, runtime_bridge::route_requests};
use super::*;
use crossbeam_channel::unbounded;
use kraai_types::SessionId;

#[test]
fn clipboard_work_does_not_block_ordered_requests_and_rejects_queued_pastes() {
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
    requests
        .send(RuntimeRequest::PasteImage { request_id: 1 })
        .unwrap();
    started
        .recv_timeout(kraai_test_support::HANG_TIMEOUT)
        .unwrap();
    requests
        .send(RuntimeRequest::PasteImage { request_id: 2 })
        .unwrap();
    requests
        .send(RuntimeRequest::CancelStream {
            session_id: SessionId::new("session"),
        })
        .unwrap();
    assert!(matches!(
        ordinary
            .recv_timeout(kraai_test_support::HANG_TIMEOUT)
            .unwrap(),
        RuntimeRequest::CancelStream { .. }
    ));
    assert!(matches!(
        results
            .recv_timeout(kraai_test_support::HANG_TIMEOUT)
            .unwrap(),
        RuntimeResponse::PasteImage {
            request_id: 2,
            result: Err(_)
        }
    ));
    release.send(()).unwrap();
    assert!(matches!(
        results
            .recv_timeout(kraai_test_support::HANG_TIMEOUT)
            .unwrap(),
        RuntimeResponse::PasteImage {
            request_id: 1,
            result: Ok(_)
        }
    ));
    drop(requests);
    assert!(router.join().is_ok());
}

#[cfg(unix)]
fn shell(script: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("sh");
    command.args(["-c", script]);
    command
}

#[cfg(unix)]
#[tokio::test]
async fn clipboard_helper_caps_both_output_streams_and_returns_complete_bytes() {
    let bytes = read_helper(
        shell("printf image; printf diagnostic >&2"),
        kraai_test_support::HANG_TIMEOUT,
        8,
    )
    .await
    .unwrap();
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
}

#[cfg(unix)]
#[tokio::test]
async fn clipboard_helper_reports_missing_image_without_an_error() {
    let command = shell(&format!(
        "exit {}",
        crate::clipboard_image::NO_IMAGE_EXIT_CODE
    ));
    assert!(
        read_helper(command, kraai_test_support::HANG_TIMEOUT, 8)
            .await
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn clipboard_helper_timeout_kills_and_reaps_the_process() {
    let child = spawn_helper(shell("exec sleep 60")).unwrap();
    let pid = child.id().expect("helper exited early");
    let result = read_child(child, Duration::from_millis(50), 8).await;
    assert!(result.is_err_and(|error| error.to_string().contains("timed out")));
    let status = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
}
