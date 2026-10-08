use kraai_persistence::SessionStore;
use std::time::Duration;

use color_eyre::eyre::Result;
use kraai_mcp::{McpConfig, McpHost, McpManager, ServerConfig, TransportConfig};
use kraai_types::McpRequest;
use serde_json::{Value, json};

use super::harness::RuntimeTestHarness;
use crate::RuntimeErrorKind;

fn config(name: &str) -> McpConfig {
    McpConfig {
        servers: [(
            name.to_owned(),
            ServerConfig {
                enabled: true,
                description: String::new(),
                startup_timeout_secs: 1,
                call_timeout_secs: 1,
                transport: TransportConfig::Stdio {
                    command: "/unused-until-discovery".into(),
                    args: Vec::new(),
                    env: [("SECRET".into(), "ephemeral-secret".into())].into(),
                    cwd: None,
                },
            },
        )]
        .into(),
        ..Default::default()
    }
}

async fn servers(harness: &RuntimeTestHarness, session: &str) -> Vec<Value> {
    let mcp = harness
        .runtime
        .agent_manager
        .read()
        .await
        .session_mcp(session);
    mcp.execute(McpRequest::Servers)
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|server| server["server"].clone())
        .collect()
}

#[tokio::test]
async fn attachments_replace_atomically_and_never_enter_session_storage() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime fixture");
    harness
        .runtime
        .agent_manager
        .write()
        .await
        .set_mcp(std::sync::Arc::new(
            McpManager::new(config("configured")).unwrap(),
        ));
    let first = harness.handle.create_session().await?;
    let second = harness.handle.create_session().await?;
    harness
        .handle
        .set_session_mcp_servers(first.clone(), config("first"))
        .await?;
    assert_eq!(
        servers(&harness, &first).await,
        vec![json!("configured"), json!("first")]
    );
    assert_eq!(servers(&harness, &second).await, vec![json!("configured")]);
    let collision = harness
        .handle
        .set_session_mcp_servers(first.clone(), config("configured"))
        .await
        .unwrap_err();
    assert_eq!(collision.kind, RuntimeErrorKind::InvalidArgument);
    assert_eq!(
        servers(&harness, &first).await,
        vec![json!("configured"), json!("first")]
    );
    harness
        .handle
        .set_session_mcp_servers(first.clone(), config("replacement"))
        .await?;
    assert_eq!(
        servers(&harness, &first).await,
        vec![json!("configured"), json!("replacement")]
    );
    let persisted = serde_json::to_string(&harness.runtime.session_store.list().await?)?;
    assert!(!persisted.contains("ephemeral-secret"));
    assert!(!persisted.contains("replacement"));
    harness
        .handle
        .set_session_mcp_servers(first.clone(), McpConfig::default())
        .await?;
    assert_eq!(servers(&harness, &first).await, vec![json!("configured")]);
    harness
        .handle
        .set_session_mcp_servers(first.clone(), config("deleted"))
        .await?;
    harness.handle.delete_session(first.clone()).await?;
    assert_eq!(servers(&harness, &first).await, vec![json!("configured")]);
    let missing = harness
        .handle
        .set_session_mcp_servers(first, config("missing"))
        .await
        .unwrap_err();
    assert_eq!(missing.kind, RuntimeErrorKind::NotFound);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn attachments_reject_preparing_and_queued_sessions_without_waiting() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime fixture");
    let session = harness.handle.create_session().await?;
    let preparation = harness
        .runtime
        .session_preparations
        .try_begin(&session)
        .expect("preparation guard");
    let manager = harness.runtime.agent_manager.write().await;
    let busy = tokio::time::timeout(
        Duration::from_secs(1),
        harness
            .handle
            .set_session_mcp_servers(session.clone(), config("attached")),
    )
    .await?;
    drop(manager);
    assert_eq!(busy.unwrap_err().kind, RuntimeErrorKind::Conflict);
    drop(preparation);
    harness.runtime.queued_messages.lock().await.insert(
        session.clone(),
        [super::super::core::QueuedMessage {
            options: Default::default(),
            message: "queued".into(),
            model_id: kraai_types::ModelId::new("mock-model"),
            provider_id: kraai_types::ProviderId::new("mock"),
        }]
        .into(),
    );
    let busy = harness
        .handle
        .set_session_mcp_servers(session.clone(), config("attached"))
        .await
        .unwrap_err();
    assert_eq!(busy.kind, RuntimeErrorKind::Conflict);
    harness
        .runtime
        .queued_messages
        .lock()
        .await
        .remove(&session);
    harness
        .handle
        .set_session_mcp_servers(session.clone(), config("attached"))
        .await?;
    assert_eq!(servers(&harness, &session).await, vec![json!("attached")]);
    harness.shutdown().await;
    Ok(())
}
