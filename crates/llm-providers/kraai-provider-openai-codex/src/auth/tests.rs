use super::token::{IdTokenClaims, normalize_plan_type};
use super::*;
use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[path = "state_tests.rs"]
mod state_tests;

#[path = "refresh_cancellation_tests.rs"]
mod refresh_cancellation_tests;

#[test]
fn request_auth_applies_subscription_headers_without_exposing_fields() {
    let auth = OpenAiCodexRequestAuth {
        access_token: String::from("access-secret"),
        account_id: String::from("account-123"),
        generation: String::from("generation"),
    };
    let request = auth
        .apply_chatgpt_headers(Client::new().get("https://chatgpt.com/backend-api/models"))
        .build()
        .unwrap();
    assert_eq!(
        request.headers().get("authorization").unwrap(),
        "Bearer access-secret"
    );
    assert_eq!(
        request.headers().get("chatgpt-account-id").unwrap(),
        "account-123"
    );
    assert_eq!(auth.account_id(), "account-123");
}
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Barrier, oneshot};
use tokio::task::AbortHandle;
use ulid::Ulid;

fn is_missing_system_ca_error(error: &dyn std::error::Error) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        let display = error.to_string();
        let debug = format!("{error:?}");
        if display.contains("No CA certificates were loaded from the system")
            || debug.contains("No CA certificates were loaded from the system")
            || display == "builder error"
        {
            return true;
        }
        current = error.source();
    }
    false
}

fn auth_controller_or_skip() -> Option<OpenAiCodexAuthController> {
    match OpenAiCodexAuthController::new_with_options(OpenAiCodexAuthControllerOptions::new(
        temp_auth_path(),
    )) {
        Ok(controller) => Some(controller),
        Err(error) if is_missing_system_ca_error(&error) => None,
        Err(error) => panic!("unexpected auth controller init error: {error}"),
    }
}

fn auth_controller_with_issuer_or_skip(
    auth_path: PathBuf,
    issuer: String,
) -> Option<OpenAiCodexAuthController> {
    let mut options = OpenAiCodexAuthControllerOptions::new(auth_path);
    options.issuer = issuer;
    match OpenAiCodexAuthController::new_with_options(options) {
        Ok(controller) => Some(controller),
        Err(error) if is_missing_system_ca_error(&error) => None,
        Err(error) => panic!("unexpected auth controller init error: {error}"),
    }
}

fn auth_controller_with_refresh_timeout_or_skip(
    auth_path: PathBuf,
    issuer: String,
    refresh_timeout: Duration,
) -> Option<OpenAiCodexAuthController> {
    let mut options = OpenAiCodexAuthControllerOptions::new(auth_path);
    options.issuer = issuer;
    let mut config = AuthConfig::from(options);
    config.refresh_timeout = refresh_timeout;
    match OpenAiCodexAuthController::with_config(config) {
        Ok(controller) => Some(controller),
        Err(error) if is_missing_system_ca_error(&error) => None,
        Err(error) => panic!("unexpected auth controller init error: {error}"),
    }
}

fn temp_auth_path() -> PathBuf {
    std::env::temp_dir()
        .join(format!("agent-openai-codex-{}", Ulid::generate()))
        .join("auth.json")
}

fn fake_jwt(email: &str, plan_type: &str, account_id: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": plan_type,
                "chatgpt_account_id": account_id
            }
        })
        .to_string(),
    );
    format!("{header}.{payload}.signature")
}

#[tokio::test]
async fn immediately_failing_login_does_not_leave_pending_state() {
    let Some(controller) = auth_controller_or_skip() else {
        return;
    };
    controller
        .install_login_task(
            OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                auth_url: String::from("https://example.invalid"),
            }),
            async { Err(io::Error::other("immediate failure")) },
        )
        .await;

    let status = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let status = controller.get_status().await;
            if status.error.is_some() {
                break status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    assert_eq!(status.state, OpenAiCodexLoginState::SignedOut);
    assert_eq!(status.error.as_deref(), Some("immediate failure"));
}

#[tokio::test]
async fn dropping_last_controller_releases_pending_browser_listener() {
    let reservation = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let mut options = OpenAiCodexAuthControllerOptions::new(temp_auth_path());
    options.default_callback_port = reservation.local_addr().unwrap().port();
    options.fallback_callback_ports.clear();
    let controller = match OpenAiCodexAuthController::new_with_options(options) {
        Ok(controller) => controller,
        Err(error) if is_missing_system_ca_error(&error) => return,
        Err(error) => panic!("unexpected auth controller init error: {error}"),
    };
    let weak = Arc::downgrade(&controller.inner);
    drop(reservation);
    let status = controller.start_browser_login().await.unwrap();
    let OpenAiCodexLoginState::BrowserPending(pending) = status.state else {
        panic!("browser login was not pending");
    };
    let auth_url = url::Url::parse(&pending.auth_url).unwrap();
    let redirect_uri = auth_url
        .query_pairs()
        .find_map(|(key, value)| (key == "redirect_uri").then(|| value.into_owned()))
        .unwrap();
    let port = url::Url::parse(&redirect_uri).unwrap().port().unwrap();
    assert!(TcpListener::bind(("127.0.0.1", port)).await.is_err());

    drop(controller);

    assert!(weak.upgrade().is_none());
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if TcpListener::bind(("127.0.0.1", port)).await.is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn completed_login_still_publishes_status_and_persists_auth() {
    let Some(controller) = auth_controller_or_skip() else {
        return;
    };
    let path = controller.inner.config.auth_path.clone();
    let auth = stored_auth("user@example.com", "pro", "workspace_123", unix_now());
    let generation = auth.generation.clone();
    let mut updates = controller.subscribe();
    controller
        .install_login_task(
            OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                auth_url: String::from("https://example.invalid"),
            }),
            async move { Ok(auth) },
        )
        .await;

    let status = tokio::time::timeout(Duration::from_secs(2), updates.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.state, OpenAiCodexLoginState::Authenticated);
    assert_eq!(
        load_auth_file(&path).unwrap().unwrap().generation,
        generation
    );
    assert_eq!(
        controller.get_request_auth().await.unwrap().generation,
        generation
    );
    assert!(controller.inner.state.lock().await.pending.is_none());
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn login_completion_holds_file_lock_until_memory_matches_disk() {
    struct CompletionWake(tokio::sync::Notify);

    impl std::task::Wake for CompletionWake {
        fn wake(self: Arc<Self>) {
            self.0.notify_one();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.notify_one();
        }
    }

    let Some(controller) = auth_controller_or_skip() else {
        return;
    };
    let path = controller.inner.config.auth_path.clone();
    let file_lock = acquire_auth_file_lock(path.clone()).await.unwrap();
    let auth = stored_auth("user@example.com", "pro", "workspace_123", unix_now());
    let generation = auth.generation.clone();
    let mut updates = controller.subscribe();
    controller
        .install_login_task(
            OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                auth_url: String::from("https://example.invalid"),
            }),
            std::future::pending(),
        )
        .await;
    let id = {
        let state = controller.inner.state.lock().await;
        let pending = state.pending.as_ref().unwrap();
        pending.task.abort();
        let id = pending.id.clone();
        drop(state);
        id
    };
    let wake = Arc::new(CompletionWake(tokio::sync::Notify::new()));
    let waker = std::task::Waker::from(wake.clone());
    let mut completion = std::pin::pin!(controller.finish_login_attempt(id, Ok(auth)));
    assert!(
        completion
            .as_mut()
            .poll(&mut std::task::Context::from_waker(&waker))
            .is_pending()
    );
    let state = controller.inner.state.lock().await;
    assert!(state.auth.is_none());
    drop(file_lock);

    tokio::time::timeout(Duration::from_secs(2), wake.0.notified())
        .await
        .unwrap();
    assert!(
        completion
            .as_mut()
            .poll(&mut std::task::Context::from_waker(&waker))
            .is_pending()
    );
    let competing_file_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.with_extension("json.refresh.lock"))
        .unwrap();
    assert!(matches!(
        competing_file_lock.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    assert!(load_auth_file(&path).unwrap().is_none());
    drop(competing_file_lock);
    drop(state);

    tokio::time::timeout(Duration::from_secs(2), completion)
        .await
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(2), updates.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.state, OpenAiCodexLoginState::Authenticated);
    assert_eq!(
        controller.get_request_auth().await.unwrap().generation,
        generation
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn refresh_failure_preserves_login_waiting_for_file_lock() {
    let Some(controller) = auth_controller_or_skip() else {
        return;
    };
    let path = controller.inner.config.auth_path.clone();
    let file_lock = acquire_auth_file_lock(path.clone()).await.unwrap();
    controller
        .install_login_task(
            OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                auth_url: String::from("https://example.invalid"),
            }),
            std::future::pending(),
        )
        .await;
    let id = {
        let state = controller.inner.state.lock().await;
        let pending = state.pending.as_ref().unwrap();
        pending.task.abort();
        let id = pending.id.clone();
        drop(state);
        id
    };
    let auth = stored_auth("user@example.com", "pro", "workspace_123", unix_now());
    let generation = auth.generation.clone();
    let mut completion = std::pin::pin!(controller.finish_login_attempt(id, Ok(auth)));
    assert!(futures::poll!(&mut completion).is_pending());
    assert!(controller.inner.login_gate.try_lock().is_err());

    controller
        .clear_auth_with_error_locked(String::from("refresh rejected"))
        .await
        .unwrap();
    drop(file_lock);
    tokio::time::timeout(Duration::from_secs(2), completion)
        .await
        .unwrap();

    let status = controller.get_status().await;
    assert_eq!(status.state, OpenAiCodexLoginState::Authenticated);
    assert!(status.error.is_none());
    assert_eq!(
        controller.get_request_auth().await.unwrap().generation,
        generation
    );
    assert_eq!(
        load_auth_file(&path).unwrap().unwrap().generation,
        generation
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn stale_login_completion_cannot_clear_newer_attempt() {
    let Some(controller) = auth_controller_or_skip() else {
        return;
    };
    controller
        .install_login_task(
            OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                auth_url: String::from("https://old.invalid"),
            }),
            std::future::pending(),
        )
        .await;
    let old_id = controller
        .inner
        .state
        .lock()
        .await
        .pending
        .as_ref()
        .unwrap()
        .id
        .clone();

    {
        let _login_guard = controller.inner.login_gate.lock().await;
        controller.cancel_pending_task_locked().await;
        controller
            .install_login_task(
                OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                    auth_url: String::from("https://new.invalid"),
                }),
                std::future::pending(),
            )
            .await;
    }
    controller
        .finish_login_attempt(old_id, Err(io::Error::other("stale failure")))
        .await;

    let status = controller.get_status().await;
    assert_eq!(
        status.state,
        OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
            auth_url: String::from("https://new.invalid"),
        })
    );
    assert!(status.error.is_none());

    let _login_guard = controller.inner.login_gate.lock().await;
    controller.cancel_pending_task_locked().await;
}

#[test]
fn id_token_claims_extract_email_plan_and_account_id() {
    let claims =
        parse_id_token_claims(&fake_jwt("user@example.com", "pro", "workspace_123")).unwrap();

    assert_eq!(claims.email.as_deref(), Some("user@example.com"));
    assert_eq!(claims.plan_type.as_deref(), Some("Pro"));
    assert_eq!(claims.account_id.as_deref(), Some("workspace_123"));
}

#[tokio::test]
async fn missing_auth_file_reports_signed_out_status() {
    let Some(controller) = auth_controller_or_skip() else {
        return;
    };

    assert_eq!(
        controller.get_status().await.state,
        OpenAiCodexLoginState::SignedOut
    );
}

#[test]
fn browser_login_url_contains_official_parameters() {
    let pkce = generate_pkce();
    let auth_url = build_authorize_url(
        AUTH_ISSUER,
        CLIENT_ID,
        "http://localhost:1455/auth/callback",
        &pkce,
        "state",
    );

    assert!(auth_url.contains("id_token_add_organizations=true"));
    assert!(auth_url.contains("codex_cli_simplified_flow=true"));
    assert!(auth_url.contains("originator=codex_cli_rs"));
}

fn stored_auth(
    email: &str,
    plan_type: &str,
    account_id: &str,
    last_refresh_unix: u64,
) -> StoredAuth {
    StoredAuth {
        tokens: StoredTokens {
            id_token: fake_jwt(email, plan_type, account_id),
            access_token: format!("access-{account_id}"),
            refresh_token: format!("refresh-{account_id}"),
            account_id: account_id.to_string(),
        },
        claims: IdTokenClaims {
            email: Some(email.to_string()),
            plan_type: Some(normalize_plan_type(plan_type)),
            account_id: Some(account_id.to_string()),
        },
        last_refresh_unix,
        generation: generate_generation(),
    }
}

async fn scripted_refresh_server(account_id: &str) -> (String, Arc<AtomicUsize>, AbortHandle) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let server_requests = requests.clone();
    let id_token = fake_jwt("refreshed@example.com", "pro", account_id);
    let account_id = account_id.to_string();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let requests = server_requests.clone();
            let id_token = id_token.clone();
            let account_id = account_id.clone();
            tokio::spawn(async move {
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).await.unwrap();
                let request_number = requests.fetch_add(1, Ordering::SeqCst);
                let (status, body) = if request_number == 0 {
                    (
                        "200 OK",
                        serde_json::json!({
                            "id_token": id_token,
                            "access_token": format!("access-refreshed-{account_id}"),
                            "refresh_token": format!("refresh-rotated-{account_id}")
                        })
                        .to_string(),
                    )
                } else {
                    (
                        "401 Unauthorized",
                        "rotated refresh token reused".to_string(),
                    )
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    (format!("http://{address}"), requests, task.abort_handle())
}

async fn stalled_refresh_server() -> (String, oneshot::Receiver<()>, AbortHandle) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (request_received, received) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await.unwrap();
        let _ = request_received.send(());
        std::future::pending::<()>().await;
    });
    (format!("http://{address}"), received, task.abort_handle())
}

async fn run_concurrent_refreshes(
    controller: OpenAiCodexAuthController,
    expected_auth: OpenAiCodexRequestAuth,
    task_count: usize,
) -> Vec<io::Result<OpenAiCodexRequestAuth>> {
    let barrier = Arc::new(Barrier::new(task_count));
    let tasks = (0..task_count).map(|_| {
        let controller = controller.clone();
        let expected_auth = expected_auth.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            controller.refresh_request_auth(&expected_auth).await
        })
    });
    futures::future::join_all(tasks)
        .await
        .into_iter()
        .map(|result| result.unwrap())
        .collect()
}

#[tokio::test]
async fn simultaneous_proactive_refreshes_use_one_network_request() {
    let path = temp_auth_path();
    persist_auth_file(
        &path,
        &stored_auth("user@example.com", "pro", "workspace_123", 0),
    )
    .unwrap();
    let (issuer, requests, server) = scripted_refresh_server("workspace_123").await;
    let Some(controller) = auth_controller_with_issuer_or_skip(path.clone(), issuer) else {
        server.abort();
        return;
    };

    let tasks = (0..8).map(|_| {
        let controller = controller.clone();
        tokio::spawn(async move { controller.get_request_auth().await })
    });
    for result in futures::future::join_all(tasks).await {
        assert_eq!(
            result.unwrap().unwrap().access_token,
            "access-refreshed-workspace_123"
        );
    }
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    server.abort();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn simultaneous_unauthorized_recovery_reuses_refreshed_credentials() {
    let path = temp_auth_path();
    persist_auth_file(
        &path,
        &stored_auth("user@example.com", "pro", "workspace_123", unix_now()),
    )
    .unwrap();
    let (issuer, requests, server) = scripted_refresh_server("workspace_123").await;
    let Some(controller) = auth_controller_with_issuer_or_skip(path.clone(), issuer) else {
        server.abort();
        return;
    };
    let expected_auth = controller.get_request_auth().await.unwrap();

    let results = run_concurrent_refreshes(controller, expected_auth, 8).await;
    for result in results {
        assert_eq!(
            result.unwrap().access_token,
            "access-refreshed-workspace_123"
        );
    }
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    server.abort();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn separate_controllers_do_not_reuse_a_rotated_refresh_token() {
    let path = temp_auth_path();
    persist_auth_file(
        &path,
        &stored_auth("user@example.com", "pro", "workspace_123", unix_now()),
    )
    .unwrap();
    let (issuer, requests, server) = scripted_refresh_server("workspace_123").await;
    let Some(first) = auth_controller_with_issuer_or_skip(path.clone(), issuer.clone()) else {
        server.abort();
        return;
    };
    let Some(second) = auth_controller_with_issuer_or_skip(path.clone(), issuer) else {
        server.abort();
        return;
    };
    let mut first_updates = first.subscribe();
    let mut second_updates = second.subscribe();
    let expected_auth = first.get_request_auth().await.unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let first_task = {
        let barrier = barrier.clone();
        let expected_auth = expected_auth.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            first.refresh_request_auth(&expected_auth).await
        })
    };
    let second_task = tokio::spawn(async move {
        barrier.wait().await;
        second.refresh_request_auth(&expected_auth).await
    });

    assert_eq!(
        first_task.await.unwrap().unwrap().account_id,
        "workspace_123"
    );
    assert_eq!(
        second_task.await.unwrap().unwrap().account_id,
        "workspace_123"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        first_updates.try_recv().unwrap().account_id.as_deref(),
        Some("workspace_123")
    );
    assert_eq!(
        second_updates.try_recv().unwrap().account_id.as_deref(),
        Some("workspace_123")
    );

    server.abort();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn stalled_refresh_releases_auth_file_lock_after_timeout() {
    let path = temp_auth_path();
    persist_auth_file(
        &path,
        &stored_auth("user@example.com", "pro", "workspace_123", unix_now()),
    )
    .unwrap();
    let (issuer, request_received, server) = stalled_refresh_server().await;
    let Some(controller) = auth_controller_with_refresh_timeout_or_skip(
        path.clone(),
        issuer,
        Duration::from_millis(100),
    ) else {
        server.abort();
        return;
    };
    let expected_auth = controller.get_request_auth().await.unwrap();

    let refresh_controller = controller.clone();
    let refresh = tokio::spawn(async move {
        refresh_controller
            .refresh_request_auth(&expected_auth)
            .await
    });
    request_received.await.unwrap();
    let mut logout = tokio::spawn(async move { controller.logout().await });
    tokio::task::yield_now().await;
    assert!(!logout.is_finished());

    let refresh_result = tokio::time::timeout(Duration::from_secs(2), refresh)
        .await
        .unwrap()
        .unwrap();
    let Err(refresh_error) = refresh_result else {
        panic!("stalled refresh unexpectedly succeeded");
    };
    let request_error = refresh_error
        .get_ref()
        .and_then(|error| error.downcast_ref::<reqwest::Error>())
        .unwrap();
    assert!(request_error.is_timeout());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), &mut logout)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .state,
        OpenAiCodexLoginState::SignedOut
    );

    server.abort();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn stale_refresh_cannot_overwrite_or_clear_newer_credentials() {
    let path = temp_auth_path();
    persist_auth_file(
        &path,
        &stored_auth("old@example.com", "pro", "workspace_old", unix_now()),
    )
    .unwrap();
    let (issuer, requests, server) = scripted_refresh_server("workspace_old").await;
    let Some(controller) = auth_controller_with_issuer_or_skip(path.clone(), issuer) else {
        server.abort();
        return;
    };
    let stale_auth = controller.get_request_auth().await.unwrap();

    controller
        .replace_auth_for_test(stored_auth(
            "new@example.com",
            "team",
            "workspace_new",
            unix_now(),
        ))
        .await
        .unwrap();
    let Err(error) = controller.refresh_request_auth(&stale_auth).await else {
        panic!("stale refresh unexpectedly succeeded");
    };

    assert_eq!(
        error.to_string(),
        "OpenAI account changed during token refresh"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    let current = controller.get_request_auth().await.unwrap();
    assert_eq!(current.account_id, "workspace_new");
    assert_eq!(current.access_token, "access-workspace_new");
    assert_eq!(
        load_auth_file(&path).unwrap().unwrap().tokens.account_id,
        "workspace_new"
    );

    server.abort();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn persisted_auth_file_round_trips() {
    let path = temp_auth_path();
    let auth = stored_auth("user@example.com", "pro", "workspace_123", 42);

    persist_auth_file(&path, &auth).unwrap();

    let loaded = load_auth_file(&path).unwrap().unwrap();
    assert_eq!(loaded.tokens.account_id, "workspace_123");
    assert_eq!(loaded.claims.email.as_deref(), Some("user@example.com"));
    assert_eq!(loaded.claims.plan_type.as_deref(), Some("Pro"));
    assert_eq!(loaded.last_refresh_unix, 42);

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn persisted_auth_file_overwrites_without_leaving_temp_files() {
    let path = temp_auth_path();

    persist_auth_file(
        &path,
        &stored_auth("first@example.com", "plus", "workspace_old", 1),
    )
    .unwrap();
    persist_auth_file(
        &path,
        &stored_auth("second@example.com", "team", "workspace_new", 2),
    )
    .unwrap();

    let loaded = load_auth_file(&path).unwrap().unwrap();
    assert_eq!(loaded.tokens.account_id, "workspace_new");
    assert_eq!(loaded.claims.email.as_deref(), Some("second@example.com"));
    assert_eq!(loaded.claims.plan_type.as_deref(), Some("Team"));
    assert_eq!(loaded.last_refresh_unix, 2);

    let temp_files = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path() != path)
        .collect::<Vec<_>>();
    assert!(temp_files.is_empty());

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn failed_auth_replacement_cleans_temp_and_preserves_destination() {
    let path = temp_auth_path();
    std::fs::create_dir_all(&path).unwrap();
    let preserved = path.join("existing");
    std::fs::write(&preserved, b"keep").unwrap();

    assert!(
        persist_auth_file(
            &path,
            &stored_auth("user@example.com", "pro", "workspace_123", 42),
        )
        .is_err()
    );

    let entries = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(entries, vec![path.clone()]);
    assert_eq!(std::fs::read(preserved).unwrap(), b"keep");
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn auth_temp_files_do_not_overwrite_existing_paths() {
    let path = temp_auth_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"keep").unwrap();

    let error = storage::create_auth_temp_file(&path).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn auth_files_are_private_at_creation_and_after_replacement() {
    use std::os::unix::fs::PermissionsExt;

    let path = temp_auth_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = storage::create_auth_temp_file(&path).unwrap();
    assert_eq!(file.metadata().unwrap().permissions().mode() & 0o077, 0);
    file.set_permissions(std::fs::Permissions::from_mode(0o644))
        .unwrap();
    drop(file);

    persist_auth_file(
        &path,
        &stored_auth("user@example.com", "pro", "workspace_123", 42),
    )
    .unwrap();

    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
