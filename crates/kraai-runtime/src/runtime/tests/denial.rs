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
        vec![ScriptedChunk::native_call(
            "call-1",
            "# timeout=30sec permissions=workspace-write\n^cargo test",
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
            Default::default(),
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
    let connection = rusqlite::Connection::open(harness.data_dir.join("kraai.sqlite3"))?;
    let (kind, id) = if fail_history {
        ("message", record.result_message_id.as_str())
    } else {
        ("execution", execution_id.as_str())
    };
    connection.execute_batch(&format!(
        "CREATE TRIGGER fail_denial BEFORE INSERT ON records WHEN NEW.kind = '{kind}' AND NEW.id = '{id}' BEGIN SELECT RAISE(FAIL, 'injected persistence failure'); END;"
    ))?;
    assert!(matches!(
        harness
            .handle
            .send_message(
                session_id.clone(),
                "queued".into(),
                "mock-model".into(),
                "mock".into(),
                Default::default(),
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
    assert!(format!("{error:?}").contains("injected persistence failure"));
    connection.execute_batch("DROP TRIGGER fail_denial")?;
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
