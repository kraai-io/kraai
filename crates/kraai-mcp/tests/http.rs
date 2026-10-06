#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "protocol fixtures use assertions and direct setup"
)]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use kraai_mcp::{McpConfig, McpHost, McpManager, ServerConfig, TransportConfig};
use kraai_types::McpRequest;
use serde_json::{Value, json};

#[derive(Default)]
struct Fixture {
    discoveries: AtomicUsize,
    lists: AtomicUsize,
    calls: AtomicUsize,
    cancellations: AtomicUsize,
}

async fn endpoint(
    State(state): State<Arc<Fixture>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    assert!(headers.get("authorization").is_none());
    let method = request["method"].as_str().unwrap_or_default();
    let id = &request["id"];
    let result = match method {
        "server/discover" => {
            state.discoveries.fetch_add(1, Ordering::SeqCst);
            json!({"resultType": "complete", "supportedVersions": ["2026-07-28"], "capabilities": {"tools": {}}, "ttlMs": 60000, "cacheScope": "private"})
        }
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
    let state = Arc::new(Fixture::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/mcp", post(endpoint))
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
                    bearer_token_env: None,
                    oauth: None,
                },
            },
        )]
        .into(),
    };
    (McpManager::new(config).unwrap(), state, task)
}

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
