use super::*;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Clone, Copy)]
enum RefreshOutcome {
    Rotated,
    Unauthorized,
    AccountChanged,
}

async fn paused_rotating_server(
    first_outcome: RefreshOutcome,
) -> (
    String,
    oneshot::Receiver<()>,
    oneshot::Sender<()>,
    Arc<AtomicUsize>,
    AbortOnDropHandle<()>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let server_requests = Arc::clone(&requests);
    let (received, request_received) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let task = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut received = Some(received);
        let mut released = Some(released);
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut length = None;
            loop {
                let mut line = String::new();
                assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':')
                    && key.eq_ignore_ascii_case("content-length")
                {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
            let mut body = vec![0; length.unwrap()];
            stream.read_exact(&mut body).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                body.get("refresh_token")
                    .and_then(serde_json::Value::as_str),
                Some("refresh-workspace_123")
            );
            let first = server_requests.fetch_add(1, Ordering::SeqCst) == 0;
            let (status, body) = if first {
                received.take().unwrap().send(()).unwrap();
                released.take().unwrap().await.unwrap();
                match first_outcome {
                    RefreshOutcome::Unauthorized => {
                        ("401 Unauthorized", String::from("refresh token expired"))
                    }
                    RefreshOutcome::Rotated | RefreshOutcome::AccountChanged => {
                        let account_id = match first_outcome {
                            RefreshOutcome::AccountChanged => "workspace_changed",
                            _ => "workspace_123",
                        };
                        (
                            "200 OK",
                            serde_json::json!({
                                "id_token": fake_jwt("refreshed@example.com", "pro", account_id),
                                "access_token": "access-rotated",
                                "refresh_token": "refresh-rotated",
                            })
                            .to_string(),
                        )
                    }
                }
            } else {
                (
                    "401 Unauthorized",
                    String::from("rotated refresh token reused"),
                )
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.get_mut().write_all(response.as_bytes()).await;
        }
    }));
    (
        format!("http://{address}"),
        request_received,
        release,
        requests,
        task,
    )
}

#[tokio::test]
async fn cancelling_a_refresh_caller_preserves_rotated_credentials() {
    let path = temp_auth_path();
    persist_auth_file(
        &path,
        &stored_auth("user@example.com", "pro", "workspace_123", unix_now()),
    )
    .unwrap();
    let (issuer, received, release, requests, _server) =
        paused_rotating_server(RefreshOutcome::Rotated).await;
    let Some(controller) = auth_controller_with_issuer_or_skip(path.clone(), issuer) else {
        return;
    };
    let expected = controller.get_request_auth().await.unwrap();
    let caller = {
        let controller = controller.clone();
        let expected = expected.clone();
        tokio::spawn(async move { controller.refresh_request_auth(&expected).await })
    };
    tokio::time::timeout(Duration::from_secs(5), received)
        .await
        .unwrap()
        .unwrap();
    caller.abort();
    assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
    release.send(()).unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        controller.refresh_request_auth(&expected),
    )
    .await
    .unwrap();
    let persisted = load_auth_file(&path).unwrap();
    let current = controller.get_request_auth().await;
    let _ = std::fs::remove_dir_all(path.parent().unwrap());

    let auth = result.unwrap();
    assert_eq!(auth.access_token, "access-rotated");
    assert_eq!(current.unwrap().access_token, "access-rotated");
    assert_eq!(persisted.unwrap().tokens.refresh_token, "refresh-rotated");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn logout_after_cancelled_refresh_cannot_restore_credentials() {
    let path = temp_auth_path();
    persist_auth_file(
        &path,
        &stored_auth("user@example.com", "pro", "workspace_123", unix_now()),
    )
    .unwrap();
    let (issuer, received, release, requests, _server) =
        paused_rotating_server(RefreshOutcome::Rotated).await;
    let Some(controller) = auth_controller_with_issuer_or_skip(path.clone(), issuer) else {
        return;
    };
    let expected = controller.get_request_auth().await.unwrap();
    let caller = {
        let controller = controller.clone();
        tokio::spawn(async move { controller.refresh_request_auth(&expected).await })
    };
    tokio::time::timeout(Duration::from_secs(5), received)
        .await
        .unwrap()
        .unwrap();
    caller.abort();
    assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
    release.send(()).unwrap();

    let status = tokio::time::timeout(Duration::from_secs(5), controller.logout())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.state, OpenAiCodexLoginState::SignedOut);
    assert!(controller.get_request_auth().await.is_err());
    assert!(load_auth_file(&path).unwrap().is_none());
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn cancelled_refresh_still_times_out_and_releases_the_file_lock() {
    let path = temp_auth_path();
    persist_auth_file(
        &path,
        &stored_auth("user@example.com", "pro", "workspace_123", unix_now()),
    )
    .unwrap();
    let (issuer, received, server) = stalled_refresh_server().await;
    let Some(controller) = auth_controller_with_refresh_timeout_or_skip(
        path.clone(),
        issuer,
        Duration::from_millis(100),
    ) else {
        server.abort();
        return;
    };
    let expected = controller.get_request_auth().await.unwrap();
    let caller = {
        let controller = controller.clone();
        tokio::spawn(async move { controller.refresh_request_auth(&expected).await })
    };
    tokio::time::timeout(Duration::from_secs(5), received)
        .await
        .unwrap()
        .unwrap();
    caller.abort();
    assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
    let result = tokio::time::timeout(Duration::from_secs(5), controller.logout()).await;
    server.abort();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert_eq!(
        result.unwrap().unwrap().state,
        OpenAiCodexLoginState::SignedOut
    );
}

async fn refresh_failure_preserves_newer_logins(outcome: RefreshOutcome) {
    for login_state in [
        OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
            auth_url: String::from("https://example.invalid"),
        }),
        OpenAiCodexLoginState::DeviceCodePending(PendingDeviceCodeLogin {
            verification_url: String::from("https://example.invalid"),
            user_code: String::from("ABCD"),
        }),
    ] {
        for cancel_caller in [false, true] {
            let path = temp_auth_path();
            persist_auth_file(
                &path,
                &stored_auth("user@example.com", "pro", "workspace_123", unix_now()),
            )
            .unwrap();
            let (issuer, received, release, requests, _server) =
                paused_rotating_server(outcome).await;
            let Some(controller) = auth_controller_with_issuer_or_skip(path.clone(), issuer) else {
                return;
            };
            let expected = controller.get_request_auth().await.unwrap();
            let caller = {
                let controller = controller.clone();
                tokio::spawn(async move { controller.refresh_request_auth(&expected).await })
            };
            tokio::time::timeout(Duration::from_secs(5), received)
                .await
                .unwrap()
                .unwrap();

            let mut updates = controller.subscribe();
            let (finish_login, login_finished) = oneshot::channel();
            controller
                .install_login_task(login_state.clone(), async move {
                    login_finished.await.map_err(io::Error::other)
                })
                .await;
            if cancel_caller {
                caller.abort();
            }
            release.send(()).unwrap();
            let status = tokio::time::timeout(Duration::from_secs(5), updates.recv())
                .await
                .unwrap()
                .unwrap();
            let result = tokio::time::timeout(Duration::from_secs(5), caller)
                .await
                .unwrap();
            if cancel_caller {
                assert!(matches!(result, Err(error) if error.is_cancelled()));
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(status.state, login_state);
            assert!(load_auth_file(&path).unwrap().is_none());

            let new_auth = stored_auth("new@example.com", "pro", "workspace_new", unix_now());
            assert!(finish_login.send(new_auth.clone()).is_ok());
            let status = tokio::time::timeout(Duration::from_secs(5), updates.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(status.state, OpenAiCodexLoginState::Authenticated);
            assert!(status.error.is_none());
            let current = controller.get_request_auth().await.unwrap();
            let persisted = load_auth_file(&path).unwrap().unwrap();
            assert_eq!(current.generation, new_auth.generation);
            assert_eq!(persisted.generation, new_auth.generation);
            assert_eq!(requests.load(Ordering::SeqCst), 1);
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
    }
}

#[tokio::test]
async fn unauthorized_refresh_preserves_newer_login() {
    refresh_failure_preserves_newer_logins(RefreshOutcome::Unauthorized).await;
}

#[tokio::test]
async fn account_changed_refresh_preserves_newer_login() {
    refresh_failure_preserves_newer_logins(RefreshOutcome::AccountChanged).await;
}
