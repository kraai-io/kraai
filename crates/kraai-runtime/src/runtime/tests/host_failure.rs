use color_eyre::eyre::Result;
use kraai_persistence::ScriptExecutionCompletion;
use kraai_types::ScriptExecutionStatus;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;
use crate::runtime::script_execution::CompletedScriptExecution;

#[tokio::test]
async fn host_failure_recovery_preserves_live_sessions_and_skips_deleted_sessions() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "<tool_call>\n# timeout=30sec permissions=workspace-write\n'changed' | save result.txt\n</tool_call>",
        )],
        vec![ScriptedChunk::plain("must not continue")],
    ])
    .await else {
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

    for expected_count in [1, 2] {
        if expected_count == 2 {
            harness.runtime.recover_script_executions().await?;
        }
        let events = harness.events.wait_for("host error in UI", |events| {
            events.iter().filter(|event| matches!(event, Event::ContinuationFailed { session_id: id, error } if id == &session_id && error == message)).count() >= expected_count
        }).await;
        assert_eq!(events.iter().filter(|event| matches!(event, Event::StreamStart { session_id: id, .. } if id == &session_id)).count(), 1);
        assert!(
            !harness
                .runtime
                .agent_manager
                .read()
                .await
                .is_turn_active(&session_id)
        );
    }
    harness.handle.delete_session(session_id).await?;
    assert_eq!(store.list_all().await?.len(), 1);
    let sequence = harness.runtime.event_tx.latest_sequence();
    harness.runtime.recover_script_executions().await?;
    assert!(harness.handle.list_sessions().await?.is_empty());
    assert_eq!(harness.runtime.event_tx.latest_sequence(), sequence);
    harness.shutdown().await;
    Ok(())
}
