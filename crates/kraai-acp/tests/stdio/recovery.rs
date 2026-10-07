use std::sync::atomic::Ordering;

use color_eyre::eyre::Result;
use kraai_persistence::ScriptExecutionStore;
use serde_json::json;

use crate::support::{Harness, Reply, prompt};

#[tokio::test]
async fn restart_after_pending_approval_waits_for_the_next_prompt() -> Result<()> {
    let mut harness = Harness::new(vec![
        Reply::Script("# timeout=10sec permissions=no-sandbox\nprint 'must-not-run'".into()),
        "After restart".into(),
    ])
    .await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness
        .send(json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":prompt(&session)}))
        .await?;
    harness
        .until(|value| value.get("method") == Some(&json!("session/request_permission")))
        .await?;
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
    assert!(!values.iter().any(|value| {
        value.pointer("/params/update/sessionUpdate") == Some(&json!("tool_call_update"))
            && value.pointer("/params/update/status") == Some(&json!("failed"))
    }));
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 1);
    let values = harness
        .request(4, "session/prompt", prompt(&session))
        .await?;
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/content/text")
                == Some(&json!("After restart")))
    );
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 2);
    let persistence =
        kraai_persistence::Persistence::open(&harness.root.path().join("state/data")).await?;
    let executions = persistence.executions().list_for_session(&session).await?;
    assert_eq!(executions.len(), 1);
    assert_eq!(
        executions.first().and_then(|execution| execution.status),
        Some(kraai_types::ScriptExecutionStatus::Cancelled)
    );
    harness.stop().await
}
