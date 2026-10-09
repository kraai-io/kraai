#![expect(
    clippy::panic_in_result_fn,
    reason = "protocol integration tests assert after fallible I/O"
)]

#[path = "stdio/commands.rs"]
mod commands;
#[path = "stdio/images.rs"]
mod images;
#[path = "stdio/lifecycle.rs"]
mod lifecycle;
#[path = "stdio/live_model_options.rs"]
mod live_model_options;
#[path = "stdio/mcp.rs"]
mod mcp;
#[path = "stdio/model_options.rs"]
mod model_options;
#[path = "stdio/models.rs"]
mod models;
#[path = "stdio/profiles.rs"]
mod profiles;
#[path = "stdio/recovery.rs"]
mod recovery;
#[path = "stdio/replay.rs"]
mod replay;
mod support;
#[path = "stdio/undo.rs"]
mod undo;
#[path = "stdio/workspace.rs"]
mod workspace;

use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};
use support::{Harness, Reply, prompt};

#[tokio::test]
async fn negotiates_streams_reloads_and_exits_on_eof() -> Result<()> {
    let mut harness = Harness::new(vec!["Hello 🦀".into(), "Continued".into()]).await?;
    let initialized = harness.initialize().await?;
    assert_eq!(
        initialized.pointer("/result/protocolVersion"),
        Some(&json!(1))
    );
    assert_eq!(
        initialized.pointer("/result/agentCapabilities/loadSession"),
        Some(&json!(true))
    );
    let session = harness.session().await?;
    let values = harness
        .request(2, "session/prompt", prompt(&session))
        .await?;
    assert_eq!(
        values
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("end_turn"))
    );
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/content/text") == Some(&json!("Hello 🦀")))
    );
    harness.restart().await?;
    harness.initialize().await?;
    let values = harness
        .request(
            3,
            "session/load",
            json!({"sessionId":session,"cwd":harness.root.path(),"mcpServers":[]}),
        )
        .await?;
    assert!(
        values
            .last()
            .and_then(|value| value.get("result"))
            .is_some()
    );
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/sessionUpdate")
                == Some(&json!("user_message_chunk")))
    );
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/content/text") == Some(&json!("Hello 🦀")))
    );
    let values = harness
        .request(4, "session/prompt", prompt(&session))
        .await?;
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/content/text") == Some(&json!("Continued")))
    );
    harness.stop().await
}

#[tokio::test]
async fn embedded_host_runs_script_and_prompt_waits_for_continuation() -> Result<()> {
    for (script, status) in [
        ("print 'host-ok'", "completed"),
        ("print 'host-ok'; exit 7", "failed"),
    ] {
        check_script(script, status).await?;
    }
    Ok(())
}

async fn check_script(script: &str, status: &str) -> Result<()> {
    let mut harness = Harness::new(vec![
        Reply::Script(format!("# timeout=10sec permissions=no-sandbox\n{script}")),
        "Finished.".into(),
    ])
    .await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness
        .send(json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":prompt(&session)}))
        .await?;
    let pending = harness
        .until(|value| value.get("method") == Some(&json!("session/request_permission")))
        .await?;
    assert!(
        !pending
            .iter()
            .any(|value| value.get("id") == Some(&json!(2)))
    );
    let approval = pending.last().ok_or_else(|| eyre!("missing approval"))?;
    harness.send(json!({"jsonrpc":"2.0","id":approval.get("id"),"result":{"outcome":{"outcome":"selected","optionId":"allow"}}})).await?;
    let values = harness
        .until(|value| value.get("id") == Some(&json!(2)))
        .await?;
    assert_eq!(
        values
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("end_turn"))
    );
    assert!(values.iter().any(|value| {
        value
            .pointer("/params/update/content/0/content/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains("host-ok"))
    }));
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/content/text") == Some(&json!("Finished.")))
    );
    assert!(!pending.iter().chain(&values).any(|value| {
        value
            .pointer("/params/update/content/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains(script))
    }));
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/status") == Some(&json!(status)))
    );
    harness.restart().await?;
    harness.initialize().await?;
    let values = harness
        .request(
            3,
            "session/load",
            json!({"sessionId":session,"cwd":harness.root.path(),"mcpServers":[]}),
        )
        .await?;
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/status") == Some(&json!(status)))
    );
    harness.stop().await
}

#[tokio::test]
async fn immediate_cancellation_is_not_lost() -> Result<()> {
    let mut harness = Harness::new(vec![Reply::Streaming]).await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness
        .send(json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":prompt(&session)}))
        .await?;
    harness
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}))
        .await?;
    let values = harness
        .until(|value| value.get("id") == Some(&json!(2)))
        .await?;
    assert_eq!(
        values
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("cancelled"))
    );
    harness.stop().await
}

#[tokio::test]
async fn validates_model_configuration() -> Result<()> {
    let mut harness = Harness::new(vec![]).await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    let values = harness
        .request(
            2,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"invalid"}),
        )
        .await?;
    assert!(values.last().and_then(|value| value.get("error")).is_some());
    let values = harness
        .request(
            3,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"4:mock:mock-model"}),
        )
        .await?;
    assert_eq!(
        values
            .last()
            .and_then(|value| value.pointer("/result/configOptions/0/currentValue")),
        Some(&json!("4:mock:mock-model"))
    );
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/sessionUpdate")
                == Some(&json!("config_option_update")))
    );
    harness.stop().await
}

#[tokio::test]
async fn cancelling_approval_stops_without_continuation_and_accepts_next_prompt() -> Result<()> {
    let mut harness = Harness::new(vec![
        Reply::Script("# timeout=10sec permissions=no-sandbox\nprint 'must-not-run'".into()),
        "After cancellation".into(),
    ])
    .await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness
        .send(json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":prompt(&session)}))
        .await?;
    let values = harness
        .until(|value| value.get("method") == Some(&json!("session/request_permission")))
        .await?;
    let approval = values.last().ok_or_else(|| eyre!("missing approval"))?;
    harness
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}))
        .await?;
    let values = harness
        .until(|value| value.get("id") == Some(&json!(2)))
        .await?;
    assert_eq!(
        values
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("cancelled"))
    );
    harness.send(json!({"jsonrpc":"2.0","id":approval.get("id"),"result":{"outcome":{"outcome":"selected","optionId":"allow"}}})).await?;
    let values = harness
        .request(3, "session/prompt", prompt(&session))
        .await?;
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/content/text")
                == Some(&json!("After cancellation")))
    );
    harness.stop().await
}

#[tokio::test]
async fn cancellation_and_concurrent_prompt_do_not_block_dispatch() -> Result<()> {
    let mut harness = Harness::new(vec![Reply::Streaming, "Next".into()]).await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness
        .send(json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":prompt(&session)}))
        .await?;
    harness
        .until(|value| value.pointer("/params/update/content/text") == Some(&json!("partial")))
        .await?;
    let values = harness
        .request(3, "session/prompt", prompt(&session))
        .await?;
    assert!(values.last().and_then(|value| value.get("error")).is_some());
    harness
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}))
        .await?;
    let values = harness
        .until(|value| value.get("id") == Some(&json!(2)))
        .await?;
    assert_eq!(
        values
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("cancelled"))
    );
    let values = harness
        .request(4, "session/prompt", prompt(&session))
        .await?;
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/content/text") == Some(&json!("Next")))
    );
    harness.stop().await
}

#[tokio::test]
async fn validates_initialization_workspace_and_mcp_transport() -> Result<()> {
    let mut harness = Harness::new(vec![]).await?;
    let values = harness
        .request(
            10,
            "session/new",
            json!({"cwd":harness.root.path(),"mcpServers":[]}),
        )
        .await?;
    assert!(values.last().and_then(|value| value.get("error")).is_some());
    harness.initialize().await?;
    for (id, params) in [
        (11, json!({"cwd":"relative","mcpServers":[]})),
        (
            12,
            json!({"cwd":harness.root.path(),"mcpServers":[{"name":"unsupported","type":"sse","url":"http://127.0.0.1/","headers":[]}]}),
        ),
    ] {
        let values = harness.request(id, "session/new", params).await?;
        assert_eq!(
            values.last().and_then(|value| value.pointer("/error/code")),
            Some(&json!(-32602))
        );
    }
    harness.session().await?;
    harness.stop().await
}
