#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "OAuth protocol fixtures assert requests and states"
)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use kraai_mcp::{McpAuthState, McpConfig, McpHost, McpManager};
use kraai_types::McpRequest;
use serde_json::{Value, json};

struct Fixture {
    base: String,
    refreshes: AtomicUsize,
    exchanges: AtomicUsize,
    calls: AtomicUsize,
    slow_metadata: AtomicBool,
    pause_refresh: AtomicBool,
    empty_secret: AtomicBool,
    refresh_started: tokio::sync::Notify,
    release_refresh: tokio::sync::Notify,
}

async fn resource(State(state): State<Arc<Fixture>>) -> Json<Value> {
    if state.slow_metadata.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
    Json(
        json!({ "resource": format!("{}/mcp", state.base), "authorization_servers": [state.base], "scopes_supported": ["tools"] }),
    )
}

async fn metadata(State(state): State<Arc<Fixture>>) -> Json<Value> {
    Json(
        json!({"issuer": state.base, "authorization_endpoint": format!("{}/authorize", state.base), "token_endpoint": format!("{}/token", state.base), "registration_endpoint": format!("{}/register", state.base), "response_types_supported": ["code"], "grant_types_supported": ["authorization_code", "refresh_token"], "code_challenge_methods_supported": ["S256"], "authorization_response_iss_parameter_supported": true, "token_endpoint_auth_methods_supported": ["client_secret_post"], "scopes_supported": ["tools", "offline_access"]}),
    )
}

async fn register(State(state): State<Arc<Fixture>>, Json(request): Json<Value>) -> Json<Value> {
    assert_eq!(request["token_endpoint_auth_method"], "none");
    assert_eq!(request["application_type"], "native");
    assert_eq!(request["scope"], "tools offline_access");
    Json(
        json!({"client_id": "test-client", "client_secret": if state.empty_secret.load(Ordering::SeqCst) { "" } else { "registration-secret" }, "redirect_uris": request["redirect_uris"], "token_endpoint_auth_method": "client_secret_post"}),
    )
}

async fn token(State(state): State<Arc<Fixture>>, body: Bytes) -> Json<Value> {
    let params: std::collections::BTreeMap<_, _> =
        url::form_urlencoded::parse(&body).into_owned().collect();
    assert_eq!(params["resource"], format!("{}/mcp", state.base));
    if state.empty_secret.load(Ordering::SeqCst) {
        assert!(!params.contains_key("client_secret"));
    } else {
        assert_eq!(params["client_secret"], "registration-secret");
    }
    let (token, refresh, expiry) = if params["grant_type"] == "authorization_code" {
        assert_eq!(params["code"], "test-code");
        assert!(params.contains_key("code_verifier"));
        state.exchanges.fetch_add(1, Ordering::SeqCst);
        ("initial", "refresh-1", 1)
    } else {
        assert_eq!(params["refresh_token"], "refresh-1");
        state.refreshes.fetch_add(1, Ordering::SeqCst);
        state.refresh_started.notify_one();
        if state.pause_refresh.load(Ordering::SeqCst) {
            state.release_refresh.notified().await;
        }
        ("renewed", "refresh-2", 3600)
    };
    Json(
        json!({"access_token": token, "refresh_token": refresh, "token_type": "Bearer", "expires_in": expiry, "scope": "tools"}),
    )
}

async fn challenge(State(state): State<Arc<Fixture>>) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            "www-authenticate",
            format!(
                "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
                state.base
            ),
        )],
    )
        .into_response()
}

async fn mcp(
    State(state): State<Arc<Fixture>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer renewed")
    {
        return challenge(State(state)).await;
    }
    let result = match request["method"].as_str().unwrap() {
        "server/discover" => {
            json!({"resultType":"complete", "supportedVersions":["2026-07-28"], "capabilities":{"tools":{}}, "ttlMs":60000, "cacheScope":"private"})
        }
        "tools/list" => {
            json!({"resultType":"complete", "tools":[{"name":"echo", "inputSchema":{"type":"object"}}], "ttlMs":60000, "cacheScope":"private"})
        }
        "tools/call" => {
            state.calls.fetch_add(1, Ordering::SeqCst);
            json!({"resultType":"complete", "content":[{"type":"text", "text":"ok"}]})
        }
        _ => return StatusCode::ACCEPTED.into_response(),
    };
    Json(json!({"jsonrpc":"2.0", "id": request["id"], "result":result})).into_response()
}

async fn fixture() -> (Arc<Fixture>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state = Arc::new(Fixture {
        base: format!("http://{}", listener.local_addr().unwrap()),
        refreshes: AtomicUsize::new(0),
        exchanges: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
        slow_metadata: AtomicBool::new(false),
        pause_refresh: AtomicBool::new(false),
        empty_secret: AtomicBool::new(false),
        refresh_started: tokio::sync::Notify::new(),
        release_refresh: tokio::sync::Notify::new(),
    });
    let router = Router::new()
        .route("/mcp", get(challenge).post(mcp))
        .route("/.well-known/oauth-protected-resource/mcp", get(resource))
        .route("/.well-known/oauth-authorization-server", get(metadata))
        .route("/register", post(register))
        .route("/token", post(token))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (state, task)
}

fn manager(base: &str, directory: &std::path::Path) -> McpManager {
    let config: McpConfig = toml::from_str(&format!("[servers.fixture]\ntransport = {{ type = 'http', url = '{base}/mcp', oauth = {{ scopes = ['tools'] }} }}")).unwrap();
    McpManager::with_auth_storage(config, directory.to_path_buf()).unwrap()
}

async fn pending(manager: &McpManager) -> url::Url {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = manager.auth_statuses().await.remove(0);
            match status.state {
                McpAuthState::Pending { auth_url } => return url::Url::parse(&auth_url).unwrap(),
                McpAuthState::Starting => tokio::time::sleep(Duration::from_millis(10)).await,
                _ => panic!("Unexpected login state: {status:?}"),
            }
        }
    })
    .await
    .unwrap()
}

async fn login(manager: &McpManager, base: &str) -> url::Url {
    assert!(matches!(
        manager.start_login("fixture").await.unwrap().state,
        McpAuthState::Starting
    ));
    let url = pending(manager).await;
    let params: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(params["code_challenge_method"], "S256");
    let mut callback = url::Url::parse(&params["redirect_uri"]).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &params["state"])
        .append_pair("code", "test-code")
        .append_pair("iss", base);
    callback
}

async fn authenticated(manager: &McpManager) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = manager.auth_statuses().await.remove(0);
            if matches!(status.state, McpAuthState::Authenticated) {
                break;
            }
            assert!(status.error.is_none(), "{status:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn login_validates_callbacks_refreshes_persists_and_logs_out() {
    let (fixture, task) = fixture().await;
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(&fixture.base, directory.path());
    assert!(!manager.prompt().await.warnings.is_empty());
    let callback = login(&manager, &fixture.base).await;
    let client = reqwest::Client::new();
    let mut wrong = callback.clone();
    wrong.query_pairs_mut().append_pair("state", "duplicate");
    assert_eq!(
        client.get(wrong).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let mut wrong = callback.clone();
    let query = wrong.query().unwrap().replace("test-code", "wrong-code");
    wrong.set_query(Some(&query.replace("iss=", "wrong_iss=")));
    assert_eq!(
        client.get(wrong).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(fixture.exchanges.load(Ordering::SeqCst), 0);
    assert!(
        client
            .get(callback)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    authenticated(&manager).await;
    let different_resource = self::manager(&format!("{}/another", fixture.base), directory.path());
    assert!(matches!(
        different_resource.auth_statuses().await[0].state,
        McpAuthState::SignedOut
    ));
    let prompt = manager.prompt().await;
    assert!(prompt.warnings.is_empty(), "{:?}", prompt.warnings);
    assert_eq!(fixture.refreshes.load(Ordering::SeqCst), 1);
    manager.shutdown().await;
    drop(manager);
    let restored = self::manager(&fixture.base, directory.path());
    assert!(matches!(
        restored.auth_statuses().await[0].state,
        McpAuthState::Authenticated
    ));
    let result = restored
        .execute(McpRequest::Call {
            server: "fixture".into(),
            tool: "echo".into(),
            arguments: Default::default(),
        })
        .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(fixture.refreshes.load(Ordering::SeqCst), 1);
    assert!(matches!(
        restored.logout("fixture").await.unwrap().state,
        McpAuthState::SignedOut
    ));
    assert!(!restored.prompt().await.warnings.is_empty());
    restored.shutdown().await;
    let restored = self::manager(&fixture.base, directory.path());
    assert!(matches!(
        restored.auth_statuses().await[0].state,
        McpAuthState::SignedOut
    ));
    task.abort();
}

#[tokio::test]
async fn cancelled_discovery_and_callbacks_cannot_restore_login() {
    let (fixture, task) = fixture().await;
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(&fixture.base, directory.path());
    fixture.slow_metadata.store(true, Ordering::SeqCst);
    manager.start_login("fixture").await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), manager.cancel_login("fixture"))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        manager.auth_statuses().await[0].state,
        McpAuthState::SignedOut
    ));
    fixture.slow_metadata.store(false, Ordering::SeqCst);
    let callback = login(&manager, &fixture.base).await;
    manager.logout("fixture").await.unwrap();
    assert!(reqwest::Client::new().get(callback).send().await.is_err());
    assert_eq!(fixture.exchanges.load(Ordering::SeqCst), 0);
    manager.shutdown().await;
    assert!(manager.start_login("fixture").await.is_err());
    task.abort();
}

#[tokio::test]
async fn cancelling_discovery_during_refresh_does_not_lose_rotated_token() {
    let (fixture, task) = fixture().await;
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(manager(&fixture.base, directory.path()));
    let callback = login(&manager, &fixture.base).await;
    reqwest::Client::new().get(callback).send().await.unwrap();
    authenticated(&manager).await;
    fixture.pause_refresh.store(true, Ordering::SeqCst);
    let discovering = manager.clone();
    let discovery = tokio::spawn(async move { discovering.prompt().await });
    tokio::time::timeout(Duration::from_secs(5), fixture.refresh_started.notified())
        .await
        .unwrap();
    discovery.abort();
    let _ = discovery.await;
    fixture.release_refresh.notify_one();
    manager.shutdown().await;
    let restored = self::manager(&fixture.base, directory.path());
    assert!(restored.prompt().await.warnings.is_empty());
    assert_eq!(fixture.refreshes.load(Ordering::SeqCst), 1);
    restored.shutdown().await;
    task.abort();
}

#[tokio::test]
async fn logout_during_refresh_cannot_restore_credentials() {
    let (fixture, task) = fixture().await;
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(manager(&fixture.base, directory.path()));
    let callback = login(&manager, &fixture.base).await;
    reqwest::Client::new().get(callback).send().await.unwrap();
    authenticated(&manager).await;
    fixture.pause_refresh.store(true, Ordering::SeqCst);
    let discovering = manager.clone();
    let discovery = tokio::spawn(async move { discovering.prompt().await });
    tokio::time::timeout(Duration::from_secs(5), fixture.refresh_started.notified())
        .await
        .unwrap();
    let logging_out = manager.clone();
    let logout = tokio::spawn(async move { logging_out.logout("fixture").await });
    fixture.release_refresh.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(5), logout)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(result.state, McpAuthState::SignedOut));
    let _ = discovery.await;
    manager.shutdown().await;
    let restored = self::manager(&fixture.base, directory.path());
    assert!(matches!(
        restored.auth_statuses().await[0].state,
        McpAuthState::SignedOut
    ));
    task.abort();
}

#[tokio::test]
async fn empty_registration_secret_remains_a_public_client_after_restart() {
    let (fixture, task) = fixture().await;
    fixture.empty_secret.store(true, Ordering::SeqCst);
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(&fixture.base, directory.path());
    let callback = login(&manager, &fixture.base).await;
    reqwest::Client::new().get(callback).send().await.unwrap();
    authenticated(&manager).await;
    manager.shutdown().await;
    let restored = self::manager(&fixture.base, directory.path());
    let prompt = restored.prompt().await;
    assert!(prompt.warnings.is_empty(), "{:?}", prompt.warnings);
    assert_eq!(fixture.refreshes.load(Ordering::SeqCst), 1);
    restored.shutdown().await;
    task.abort();
}
