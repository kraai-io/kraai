// Clippy only applies its allow-*-in-tests settings to helpers under a `cfg(test)` ancestor.
#![cfg(test)]

use axum::{
    Router,
    body::{Body, Bytes},
    http::StatusCode,
    routing::{get, post},
};
use futures::{StreamExt, stream};
use kraai_headless::{RunOptions, RunOutcome};
use serde_json::json;
use tokio_util::task::AbortOnDropHandle;

async fn run_against(chat_completions: Router) -> (RunOutcome, String) {
    let root = tempfile::tempdir().unwrap();
    let router = Router::new()
        .route(
            "/models",
            get(async || axum::Json(json!({"data":[{"id":"mock-model"}]}))),
        )
        .merge(chat_completions);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let _server =
        AbortOnDropHandle::new(tokio::spawn(
            async move { axum::serve(listener, router).await },
        ));
    tokio::fs::write(
        root.path().join("providers.toml"),
        format!(
            r#"
[[provider]]
id = "mock"
type = "openai-chat-completions"
base_url = "http://{address}"
api_key = "test"
only_listed_models = true

[[model]]
id = "mock-model"
provider_id = "mock"
"#
        ),
    )
    .await
    .unwrap();
    let runtime = kraai_runtime::RuntimeBuilder::new(kraai_providers::DefaultProviders)
        .storage_root(root.path().join("state"))
        .provider_config_path(root.path().join("providers.toml"))
        .mcp_config_path(root.path().join("mcp.toml"))
        .build_on(&tokio::runtime::Handle::current());
    let mut output = Vec::new();
    let outcome = kraai_headless::run(
        &runtime,
        RunOptions {
            provider_id: "mock".into(),
            model_id: "mock-model".into(),
            options: Vec::new(),
            agent_profile: "coding".into(),
            message: "hi".into(),
        },
        &mut output,
    )
    .await
    .unwrap();
    runtime.shutdown().await.unwrap();
    (outcome, String::from_utf8(output).unwrap())
}

#[tokio::test]
async fn a_prompt_runs_to_completion_and_reports_its_metrics() {
    let (RunOutcome { metrics, failure }, output) = run_against(Router::new().route(
        "/chat/completions",
        post(async || {
            let chunks = [
                json!({"choices":[{"index":0,"delta":{"content":"Hello "}}]}),
                json!({"choices":[{"index":0,"delta":{"content":"there."},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}}),
            ];
            let body =
                stream::iter(chunks.map(|chunk| {
                    Ok::<_, std::io::Error>(Bytes::from(format!("data: {chunk}\n\n")))
                }))
                .chain(stream::iter([Ok(Bytes::from_static(b"data: [DONE]\n\n"))]));
            (
                [("content-type", "text/event-stream")],
                Body::from_stream(body),
            )
        }),
    ))
    .await;
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(output, "Hello there.");
    assert_eq!(metrics.turns, Some(1));
    assert_eq!(metrics.script_executions, Some(0));
    assert_eq!(metrics.usage.map(|usage| usage.total_tokens), Some(15));
    assert_eq!(metrics.request_costs.len(), 1);
}

#[tokio::test]
async fn a_rejected_response_fails_the_run() {
    let (RunOutcome { failure, .. }, _) = run_against(Router::new().route(
        "/chat/completions",
        post(async || (StatusCode::BAD_REQUEST, "rejected by the provider")),
    ))
    .await;
    assert!(
        failure.is_some(),
        "a run whose response failed reported success"
    );
}
