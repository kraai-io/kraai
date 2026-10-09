use color_eyre::eyre::Result;
use kraai_persistence::ScriptExecutionCompletion;
use kraai_types::ScriptExecutionStatus;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;
use crate::runtime::script_execution::CompletedScriptExecution;

#[tokio::test]
async fn host_failure_recovery_preserves_live_sessions_and_skips_deleted_sessions() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::native_call(
            "call-1",
            "# timeout=30sec permissions=workspace-write\n'changed' | save result.txt",
        )],
        vec![ScriptedChunk::plain("must not continue")],
    ])
    .await
    else {
        return Ok(());
    };
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            "change it".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    harness.events.wait_for("script approval", |events| {
        events.iter().any(|event| matches!(event, Event::ScriptApprovalRequested { session_id: id, .. } if id == &session_id))
    }).await;
    let pending = harness
        .runtime
        .pending_script_approvals
        .lock()
        .await
        .remove(&session_id)
        .expect("pending script");
    let store = &harness.runtime.execution_store;
    let message = "Incompatible Nushell sibling; rebuild both binaries with `just build`";
    let record = store
        .finish(
            &pending.request.id,
            ScriptExecutionCompletion {
                status: ScriptExecutionStatus::HostUnavailable,
                exit_code: None,
                sandbox_denied: false,
                error: Some(message.into()),
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        )
        .await?;
    let output = store.read_output(&record.id).await?;
    harness
        .runtime
        .finalize_script_turn(&session_id, CompletedScriptExecution { record, output })
        .await?;

    harness.events.wait_for("host error in UI", |events| {
        events.iter().any(|event| matches!(event, Event::ContinuationFailed { session_id: id, error } if id == &session_id && error == message))
    }).await;
    harness
        .runtime
        .session_store
        .claim_turn(&session_id)
        .await?;
    harness
        .runtime
        .recover_session_executions(&session_id)
        .await?;
    assert!(
        !harness
            .runtime
            .agent_manager
            .read()
            .await
            .is_turn_active(&session_id)
    );
    harness.handle.delete_session(session_id.clone()).await?;
    assert!(store.list_all().await?.is_empty());
    assert!(harness.handle.list_sessions().await?.is_empty());
    harness.shutdown().await;
    Ok(())
}
