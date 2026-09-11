use std::sync::Arc;
use std::time::{Duration, Instant};

use color_eyre::eyre::Result;
use kraai_types::ScriptExecutionId;
use tokio::sync::mpsc;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::handle::Command;
use crate::{Event, SubmitMessageOutcome};

#[tokio::test]
async fn denial_execution_failure_cleans_up_turn_and_drains_queue() -> Result<()> {
    denial_failure_cleans_up(false).await
}

#[tokio::test]
async fn denial_history_failure_cleans_up_turn_and_drains_queue() -> Result<()> {
    denial_failure_cleans_up(true).await
}

async fn denial_failure_cleans_up(fail_history: bool) -> Result<()> {
    let harness = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "<tool_call>\n# timeout=30sec permissions=workspace-write\n^cargo test\n</tool_call>",
        )],
        vec![ScriptedChunk::plain("Queued message handled")],
    ])
    .await
    .expect("runtime fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            "first".into(),
            "mock-model".into(),
            "mock".into(),
        )
        .await?;
    harness
        .events
        .wait_for("approval", |events| {
            events.iter().any(|event| {
                matches!(event,
                    Event::ScriptApprovalRequested { session_id: id, .. } if id == &session_id
                )
            })
        })
        .await;
    let pending = harness
        .handle
        .get_pending_script(session_id.clone())
        .await?
        .expect("pending approval");
    let execution_id = ScriptExecutionId::new(pending.execution_id);
    let record = harness
        .runtime
        .execution_store
        .get(&execution_id)
        .await?
        .expect("execution record");
    let blocked_path = if fail_history {
        harness
            .data_dir
            .join("messages")
            .join(format!("{}.json", record.result_message_id))
    } else {
        let path = harness
            .data_dir
            .join("executions")
            .join(execution_id.as_str())
            .join("stdout.bin");
        tokio::fs::remove_file(&path).await?;
        path
    };
    tokio::fs::create_dir(&blocked_path).await?;
    assert!(matches!(
        harness
            .handle
            .send_message(
                session_id.clone(),
                "queued".into(),
                "mock-model".into(),
                "mock".into()
            )
            .await?,
        SubmitMessageOutcome::Queued { .. }
    ));

    let mut runtime = harness.runtime.clone();
    runtime.queue_drains = Arc::default();
    let guard = runtime.session_state_barrier.read().await;
    let error = runtime
        .deny_pending_script(session_id.clone(), execution_id)
        .await
        .expect_err("persistence must fail");
    assert!(
        error
            .chain()
            .any(|cause| cause.downcast_ref::<std::io::Error>().is_some())
    );
    drop(guard);
    let snapshot = runtime.build_session_snapshot(&session_id).await?;
    assert!(!snapshot.session.is_running);
    assert!(snapshot.pending_script.is_none());
    assert!(snapshot.turn_timer.elapsed(Instant::now()).is_none());
    assert!(snapshot.turn_timer.last_duration().is_some());
    assert_eq!(snapshot.queued_messages, 1);
    harness
        .events
        .wait_for("denial failure", |events| {
            events.iter().any(|event| {
                matches!(event,
                    Event::ContinuationFailed { session_id: id, .. } if id == &session_id
                )
            })
        })
        .await;

    let (_tx, mut rx) = mpsc::channel(1);
    let command = tokio::time::timeout(Duration::from_secs(1), runtime.next_command(&mut rx))
        .await?
        .expect("queue drain command");
    assert!(
        matches!(&command, Command::StartQueuedMessages { session_id: id } if id == &session_id)
    );
    runtime.handle_command(command).await?;
    harness.events.wait_for("queued response", |events| events.iter().any(|event| matches!(event,
        Event::StreamChunk { session_id: id, chunk, .. } if id == &session_id && chunk == "Queued message handled"
    ))).await;
    harness.shutdown().await;
    Ok(())
}
