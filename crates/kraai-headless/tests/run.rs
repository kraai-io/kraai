use axum::{
    Router,
    body::{Body, Bytes},
    routing::{get, post},
};
use color_eyre::eyre::{Result, ensure};
use futures::{StreamExt, stream};
use kraai_headless::{RunOptions, RunOutcome};
use serde_json::json;
use tokio_util::task::AbortOnDropHandle;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions follow a fallible run against a local provider"
)]
async fn a_prompt_runs_to_completion_and_reports_its_metrics() -> Result<()> {
    let root = tempfile::tempdir()?;
    let router = Router::new()
        .route("/models", get(async || axum::Json(json!({"data":[{"id":"mock-model"}]}))))
        .route("/chat/completions", post(async || {
            let chunks = [
                json!({"choices":[{"index":0,"delta":{"content":"Hello "}}]}),
                json!({"choices":[{"index":0,"delta":{"content":"there."},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}}),
            ];
            let body = stream::iter(chunks.map(|chunk| Ok::<_, std::io::Error>(Bytes::from(format!("data: {chunk}\n\n")))))
                .chain(stream::iter([Ok(Bytes::from_static(b"data: [DONE]\n\n"))]));
            ([("content-type", "text/event-stream")], Body::from_stream(body))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
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
    .await?;
    let runtime = kraai_runtime::RuntimeBuilder::new(kraai_providers::DefaultProviders)
        .storage_root(root.path().join("state"))
        .provider_config_path(root.path().join("providers.toml"))
        .mcp_config_path(root.path().join("mcp.toml"))
        .build_on(&tokio::runtime::Handle::current());
    let mut output = Vec::new();
    let RunOutcome { metrics, failure } = kraai_headless::run(
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
    .await?;
    runtime.shutdown().await?;
    ensure!(failure.is_none(), "{failure:?}");
    assert_eq!(String::from_utf8(output)?, "Hello there.");
    assert_eq!(metrics.turns, Some(1));
    assert_eq!(metrics.script_executions, Some(0));
    assert_eq!(metrics.usage.map(|usage| usage.total_tokens), Some(15));
    assert_eq!(metrics.request_costs.len(), 1);
    Ok(())
}
