use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::task::AbortOnDropHandle;

use crate::support::{Harness, prompt};

pub struct HttpFixture {
    pub url: String,
    pub calls: Arc<Mutex<Vec<Value>>>,
    _task: AbortOnDropHandle<()>,
}

#[derive(Clone)]
struct HttpState {
    marker: String,
    calls: Arc<Mutex<Vec<Value>>>,
}

impl HttpFixture {
    pub async fn new(marker: &str) -> Result<Self> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let state = HttpState {
            marker: marker.into(),
            calls: calls.clone(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/mcp", listener.local_addr()?);
        let router = Router::new()
            .route("/mcp", post(endpoint))
            .with_state(state);
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        }));
        Ok(Self {
            url,
            calls,
            _task: task,
        })
    }

    pub fn config(&self) -> Value {
        json!({"type":"http","name":"attached","url":self.url,"headers":[{"name":"Authorization","value":"Bearer acp-test-secret"},{"name":"X-ACP-Fixture","value":"header value"}]})
    }
}

async fn endpoint(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer acp-test-secret")
        || headers
            .get("x-acp-fixture")
            .and_then(|value| value.to_str().ok())
            != Some("header value")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let result = match request.get("method").and_then(Value::as_str) {
        Some("server/discover") => json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":60000,"cacheScope":"private"}),
        Some("tools/list") => json!({"resultType":"complete","ttlMs":60000,"cacheScope":"private","tools":[{"name":"echo","description":state.marker,"inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]}),
        Some("tools/call") => {
            state.calls.lock().await.push(request.clone());
            json!({"resultType":"complete","content":[{"type":"text","text":state.marker}],"structuredContent":request.pointer("/params/arguments")})
        }
        _ => return Json(json!({"jsonrpc":"2.0","id":request.get("id"),"error":{"code":-32601,"message":"unsupported"}})).into_response(),
    };
    Json(json!({"jsonrpc":"2.0","id":request.get("id"),"result":result})).into_response()
}

pub async fn session(harness: &mut Harness, servers: Value) -> Result<String> {
    let values = harness
        .request(
            10,
            "session/new",
            json!({"cwd":harness.root.path(),"mcpServers":servers}),
        )
        .await?;
    values
        .last()
        .and_then(|value| value.pointer("/result/sessionId"))
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| eyre!("MCP session creation failed: {values:?}"))
}

pub async fn load(harness: &mut Harness, session: &str, servers: Value) -> Result<()> {
    let values = harness
        .request(
            11,
            "session/load",
            json!({"sessionId":session,"cwd":harness.root.path(),"mcpServers":servers}),
        )
        .await?;
    color_eyre::eyre::ensure!(
        values
            .last()
            .is_some_and(|value| value.get("result").is_some()),
        "MCP session load failed: {values:?}"
    );
    Ok(())
}

pub async fn execute(harness: &mut Harness, session: &str) -> Result<Vec<Value>> {
    harness
        .send(json!({"jsonrpc":"2.0","id":100,"method":"session/prompt","params":prompt(session)}))
        .await?;
    let mut values = Vec::new();
    loop {
        let value = harness.read().await?;
        if value.get("method") == Some(&json!("session/request_permission")) {
            harness.send(json!({"jsonrpc":"2.0","id":value.get("id"),"result":{"outcome":{"outcome":"selected","optionId":"allow"}}})).await?;
        }
        let done = value.get("id") == Some(&json!(100)) && value.get("method").is_none();
        values.push(value);
        if done {
            break;
        }
    }
    color_eyre::eyre::ensure!(
        values
            .last()
            .and_then(|value| value.pointer("/result/stopReason"))
            == Some(&json!("end_turn")),
        "MCP prompt failed: {values:?}"
    );
    Ok(values)
}

pub fn tool_status(values: &[Value], status: &str) -> bool {
    values
        .iter()
        .any(|value| value.pointer("/params/update/status") == Some(&json!(status)))
}

pub const STDIO: &str = r#"
printf '%s\n' "$$" > "$ACP_MCP_PID"
printf '%s\n' "$PWD" "$ACP_MCP_VALUE" > "$ACP_MCP_RECORD"
while IFS= read -r line; do
    id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
    case "$line" in
        *server/discover*) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"legacy"}}\n' "$id" ;;
        *initialize\"*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"acp-fixture","version":"1.0.0"}}}\n' "$id" ;;
        *tools/list*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"ACP stdio attached echo","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
        *tools/call*) printf 'called\n' >> "$ACP_MCP_RECORD"; printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$id" "$ACP_MCP_VALUE" ;;
    esac
done
"#;
