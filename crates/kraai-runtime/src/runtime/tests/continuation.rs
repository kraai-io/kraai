use kraai_execution_types::ScriptExecutionStatus;
use kraai_persistence::ScriptExecutionCompletion;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;
use crate::runtime::script_execution::CompletedScriptExecution;

#[tokio::test]
async fn unreadable_queue_after_a_script_ends_the_turn() {
    let harness = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::native_call(
            "call-1",
            "# timeout=30sec permissions=workspace-write\nprint hello",
        )],
        vec![ScriptedChunk::plain("must not continue")],
    ])
    .await;
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await;
    harness
        .handle
        .send_message(
            session_id.clone(),
            "run it".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await
        .unwrap();
    harness.events.wait_for("script approval", |events| {
        events.iter().any(|event| matches!(event, Event::ScriptApprovalRequested { session_id: id, .. } if id == &session_id))
    }).await;
    rusqlite::Connection::open(harness.data_dir.join("kraai.sqlite3"))
        .unwrap()
        .execute(
            "INSERT INTO queued_messages(session_id, sequence, data) VALUES (?1, 1, 'not json')",
            [session_id.as_str()],
        )
        .unwrap();
    harness
        .runtime
        .with_session_actor(&session_id, move |actor| {
            Box::pin(async move {
                let pending = actor.take_pending_approval().expect("pending script");
                let store = actor.runtime.execution_store.clone();
                store.mark_running(&pending.request.id).await?;
                let record = store
                    .finish(
                        &pending.request.id,
                        ScriptExecutionCompletion {
                            status: ScriptExecutionStatus::Completed,
                            exit_code: Some(0),
                            sandbox_denied: false,
                            error: None,
                            stdout: b"hello\n".to_vec(),
                            stderr: Vec::new(),
                        },
                    )
                    .await?;
                let output = store.read_output(&record.id).await?;
                actor
                    .finish_script(CompletedScriptExecution { record, output })
                    .await
            })
        })
        .await
        .unwrap();
    harness.events.wait_for("continuation failure", |events| {
        events.iter().any(|event| matches!(event, Event::ContinuationFailed { session_id: id, .. } if id == &session_id))
    }).await;
    assert!(!harness.runtime.agent_manager.is_turn_active(&session_id));
    assert!(
        !harness
            .runtime
            .session_store
            .owns_turn(&session_id)
            .await
            .unwrap()
    );
    harness.shutdown().await;
}
