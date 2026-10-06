#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "protocol fixtures use assertions and direct setup"
)]

use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::post,
};
use kraai_mcp::{McpConfig, McpHost, McpManager, ServerConfig, TransportConfig};
use kraai_types::McpRequest;
use serde_json::{Value, json};

#[derive(Default)]
struct Fixture {
    url: String,
    headers: BTreeMap<String, String>,
    legacy: bool,
    reject_discovery: Option<StatusCode>,
    discoveries: AtomicUsize,
    initializations: AtomicUsize,
    initialize_versions: std::sync::Mutex<Vec<Value>>,
    lists: AtomicUsize,
    calls: AtomicUsize,
    cancellations: AtomicUsize,
}

async fn endpoint(
    State(state): State<Arc<Fixture>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    for (name, expected) in &state.headers {
        assert_eq!(
            headers.get(name).and_then(|value| value.to_str().ok()),
            Some(expected.as_str())
        );
    }
    if !state
        .headers
        .keys()
        .any(|name| name.eq_ignore_ascii_case("authorization"))
    {
        assert!(headers.get("authorization").is_none());
    }
    let method = request["method"].as_str().unwrap_or_default();
    let id = &request["id"];
    let result = match method {
        "server/discover" => {
            state.discoveries.fetch_add(1, Ordering::SeqCst);
            if let Some(status) = state.reject_discovery { return status.into_response(); }
            if state.legacy {
                return (StatusCode::BAD_REQUEST, Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":rmcp::model::ErrorCode::UNSUPPORTED_PROTOCOL_VERSION,"message":"Unsupported protocol version","data":{"supported":["2025-06-18"]}}}))).into_response();
            }
            json!({"resultType": "complete", "supportedVersions": ["2026-07-28"], "capabilities": {"tools": {}}, "ttlMs": 60000, "cacheScope": "private"})
        }
        "initialize" => {
            state.initializations.fetch_add(1, Ordering::SeqCst);
            state.initialize_versions.lock().unwrap().push(request["params"]["protocolVersion"].clone());
            if let Some(status) = state.reject_discovery { return status.into_response(); }
            json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"legacy-fixture","version":"1.0.0"}})
        }
        "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
        "tools/list" => {
            state.lists.fetch_add(1, Ordering::SeqCst);
            let second = request["params"]["cursor"] == "next";
            let mut page = json!({"resultType": "complete", "ttlMs": 60000, "cacheScope": "private", "tools": [{
                "name": if second { "slow" } else { "echo" }, "description": "Echo a record",
                "inputSchema": {"type": "object", "properties": {"value": {}}, "required": ["value"]}
            }]});
            if !second { page["nextCursor"] = json!("next"); }
            page
        }
        "tools/call" => {
            state.calls.fetch_add(1, Ordering::SeqCst);
            if request["params"]["name"] == "bad-request" { return StatusCode::BAD_REQUEST.into_response(); }
            if request["params"]["name"] == "slow" {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            json!({"resultType": "complete", "content": [{"type": "text", "text": "echoed"}], "structuredContent": request["params"]["arguments"]})
        }
        "notifications/cancelled" => {
            state.cancellations.fetch_add(1, Ordering::SeqCst);
            return StatusCode::ACCEPTED.into_response();
        }
        _ => return Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Unknown method"}})).into_response(),
    };
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

async fn fixture() -> (McpManager, Arc<Fixture>, tokio::task::JoinHandle<()>) {
    fixture_with_headers(BTreeMap::new()).await
}

async fn fixture_with_headers(
    headers: BTreeMap<String, String>,
) -> (McpManager, Arc<Fixture>, tokio::task::JoinHandle<()>) {
    fixture_with_protocol(headers, false, None).await
}

async fn fixture_with_protocol(
    headers: BTreeMap<String, String>,
    legacy: bool,
    reject_discovery: Option<StatusCode>,
) -> (McpManager, Arc<Fixture>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let state = Arc::new(Fixture {
        url: url.clone(),
        headers: headers.clone(),
        legacy,
        reject_discovery,
        ..Default::default()
    });
    let router = Router::new()
        .route("/mcp", post(endpoint))
        .route("/redirect", post(async || Redirect::temporary("/mcp")))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let config = McpConfig {
        prompt_max_bytes: 16384,
        servers: [(
            "fixture".into(),
            ServerConfig {
                enabled: true,
                description: "Fixture".into(),
                startup_timeout_secs: 2,
                call_timeout_secs: 1,
                transport: TransportConfig::Http {
                    url,
                    headers,
                    bearer_token_env: None,
                    oauth: None,
                },
            },
        )]
        .into(),
    };
    (McpManager::new(config).unwrap(), state, task)
}

#[path = "http/legacy.rs"]
mod legacy;
#[path = "http/session.rs"]
mod session;

#[tokio::test]
async fn http_discovery_paginates_caches_and_preserves_structured_results() {
    let (manager, state, task) = fixture().await;
    let prompt = manager.prompt().await;
    assert!(prompt.warnings.is_empty(), "{:?}", prompt.warnings);
    assert!(prompt.text.unwrap().contains("inputSchema"));
    let results = manager
        .execute(McpRequest::Search {
            query: "echo".into(),
            limit: 5,
        })
        .await
        .unwrap();
    assert_eq!(results["tools"].as_array().unwrap().len(), 2);
    assert_eq!(state.discoveries.load(Ordering::SeqCst), 1);
    assert_eq!(state.lists.load(Ordering::SeqCst), 2);
    let arguments = json!({"value": {"rows": [null, true, 42, "å"]}})
        .as_object()
        .unwrap()
        .clone();
    let result = manager
        .execute(McpRequest::Call {
            server: "fixture".into(),
            tool: "echo".into(),
            arguments: arguments.clone(),
        })
        .await
        .unwrap();
    assert_eq!(result["structuredContent"], json!(arguments));
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    manager.shutdown().await;
    task.abort();
}

#[tokio::test]
async fn timed_out_calls_are_not_replayed() {
    let (manager, state, task) = fixture().await;
    let result = manager
        .execute(McpRequest::Call {
            server: "fixture".into(),
            tool: "slow".into(),
            arguments: Default::default(),
        })
        .await;
    assert!(result.unwrap_err().contains("may have completed"));
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    manager.shutdown().await;
    task.abort();
}

#[tokio::test]
async fn dropping_a_call_does_not_replay_it_or_break_the_connection() {
    let (manager, state, task) = fixture().await;
    let manager = Arc::new(manager);
    let calling = manager.clone();
    let call = tokio::spawn(async move {
        calling
            .execute(McpRequest::Call {
                server: "fixture".into(),
                tool: "slow".into(),
                arguments: Default::default(),
            })
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while state.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    call.abort();
    let _ = call.await;
    let result = manager
        .execute(McpRequest::Call {
            server: "fixture".into(),
            tool: "echo".into(),
            arguments: Default::default(),
        })
        .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(state.calls.load(Ordering::SeqCst), 2);
    manager.shutdown().await;
    task.abort();
}
