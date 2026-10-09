use color_eyre::eyre::Result;
use futures::poll;
use kraai_persistence::ScriptExecutionCompletion;
use kraai_types::ScriptExecutionStatus;
use tokio_util::sync::CancellationToken;

use super::harness::{
    RuntimeTestHarness, ScriptedChunk, TEST_TIMEOUT, create_session_with_profile,
};
use crate::Event;
use crate::runtime::core::ActiveScriptTask;
use crate::runtime::script_execution::CompletedScriptExecution;

async fn finished_execution() -> Result<(RuntimeTestHarness, String, CompletedScriptExecution)> {
    let harness = RuntimeTestHarness::new(vec![vec![ScriptedChunk::native_call(
        "cancel-call",
        "# timeout=30sec permissions=workspace-write\n'changed' | save result.txt",
    )]])
    .await
    .expect("runtime regression fixture must initialize");
    let session = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session.clone(),
            "change it".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    harness.events.wait_for("script approval", |events| {
        events.iter().any(|event| matches!(event, Event::ScriptApprovalRequested { session_id, .. } if session_id == &session))
    }).await;
    let pending = harness
        .runtime
        .pending_script_approvals
        .lock()
        .await
        .remove(&session)
        .expect("pending script");
    harness
        .runtime
        .execution_store
        .mark_running(&pending.request.id)
        .await?;
    let record = harness
        .runtime
        .execution_store
        .finish(
            &pending.request.id,
            ScriptExecutionCompletion {
                status: ScriptExecutionStatus::Completed,
                exit_code: Some(0),
                sandbox_denied: false,
                error: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        )
        .await?;
    let output = harness
        .runtime
        .execution_store
        .read_output(&record.id)
        .await?;
    Ok((
        harness,
        session,
        CompletedScriptExecution { record, output },
    ))
}

#[tokio::test]
async fn cancellation_after_execution_finishes_waits_for_result_and_prevents_continuation()
-> Result<()> {
    let (harness, session, completed) = finished_execution().await?;
    let result_id = completed.record.result_message_id.clone();
    let cancellation = CancellationToken::new();
    let completion = CancellationToken::new();
    let completion_guard = completion.clone().drop_guard();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let runtime = harness.runtime.clone();
    let task_session = session.clone();
    let task = tokio::spawn(async move {
        let _completion_guard = completion_guard;
        finish_rx.await.expect("release finalization");
        let _state = runtime.session_state_barrier.read().await;
        let _preparation = runtime.session_preparations.begin(&task_session).await;
        runtime
            .active_script_tasks
            .lock()
            .await
            .remove(&task_session);
        runtime
            .finalize_script_turn(&task_session, completed)
            .await
            .expect("persist completed result");
    });
    harness.runtime.active_script_tasks.lock().await.insert(
        session.clone(),
        ActiveScriptTask {
            cancellation: cancellation.clone(),
            completion: completion.clone(),
            join_handle: task,
        },
    );
    let runtime = harness.runtime.clone();
    let cancel = runtime.cancel_stream(session.clone());
    tokio::pin!(cancel);
    assert!(poll!(&mut cancel).is_pending());
    assert!(cancellation.is_cancelled());
    assert!(!completion.is_cancelled());
    assert!(!runtime.agent_manager.read().await.is_turn_active(&session));
    let observer = kraai_persistence::Persistence::open(&harness.data_dir).await?;
    assert!(observer.sessions().claim_turn(&session).await.is_err());
    finish_tx.send(()).expect("release finalization");
    assert!(tokio::time::timeout(TEST_TIMEOUT, cancel).await??);
    assert!(completion.is_cancelled());
    assert!(!runtime.session_store.owns_turn(&session).await?);
    let snapshot = runtime.build_session_snapshot(&session).await?;
    assert!(!snapshot.session.is_running);
    assert!(snapshot.history.values().any(|message| matches!(
        message.content,
        kraai_types::ConversationItem::ScriptResult { .. }
    )));
    let sequence = runtime.event_tx.latest_sequence();
    runtime.spawn_continuation(session, result_id);
    tokio::task::yield_now().await;
    assert!(runtime.active_streams.lock().await.is_empty());
    assert_eq!(runtime.event_tx.latest_sequence(), sequence);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn cancellation_waits_for_continuation_stream_registration() -> Result<()> {
    let (harness, session, completed) = finished_execution().await?;
    harness
        .runtime
        .agent_manager
        .write()
        .await
        .add_script_result_to_history(
            &session,
            completed.record.result_message_id.clone(),
            completed.record.profile.id.clone(),
            completed.record.call_id.clone(),
            completed.render_result()?,
            completed.record.outcome()?,
        )
        .await?;
    let streams = harness.runtime.active_streams.lock().await;
    let runtime = harness.runtime.clone();
    let task_session = session.clone();
    let continuation = tokio::spawn(async move {
        runtime
            .start_continuation(
                task_session,
                kraai_types::ModelId::new("mock-model"),
                kraai_types::ProviderId::new("mock"),
                Default::default(),
            )
            .await
    });
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if harness
                .runtime
                .agent_manager
                .read()
                .await
                .streaming_session_ids()
                .await
                .contains(&session)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert!(harness.runtime.session_preparations.is_active(&session));
    let runtime = harness.runtime.clone();
    let cancel = runtime.cancel_stream(session.clone());
    tokio::pin!(cancel);
    assert!(poll!(&mut cancel).is_pending());
    drop(streams);
    assert_eq!(
        tokio::time::timeout(TEST_TIMEOUT, continuation).await???,
        crate::ContinueSessionOutcome::Started
    );
    tokio::time::timeout(TEST_TIMEOUT, cancel).await??;
    assert!(runtime.active_streams.lock().await.is_empty());
    assert!(!runtime.agent_manager.read().await.is_turn_active(&session));
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn stale_continuation_cannot_resume_a_new_turn() -> Result<()> {
    let (harness, session, completed) = finished_execution().await?;
    harness
        .runtime
        .agent_manager
        .write()
        .await
        .add_script_result_to_history(
            &session,
            completed.record.result_message_id.clone(),
            completed.record.profile.id.clone(),
            completed.record.call_id.clone(),
            completed.render_result()?,
            completed.record.outcome()?,
        )
        .await?;
    let preparation = harness.runtime.session_preparations.begin(&session).await;
    harness
        .runtime
        .spawn_continuation(session.clone(), completed.record.result_message_id);
    tokio::time::timeout(TEST_TIMEOUT, async {
        while harness.runtime.session_state_barrier.try_write().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let mut agent = harness.runtime.agent_manager.write().await;
    agent.clear_active_turn(&session);
    let request = agent
        .prepare_start_stream(
            &session,
            "new turn".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
            Default::default(),
        )
        .await?;
    agent.complete_message(&request.message_id).await?;
    drop(agent);
    let sequence = harness.runtime.event_tx.latest_sequence();
    drop(preparation);
    let state =
        tokio::time::timeout(TEST_TIMEOUT, harness.runtime.session_state_barrier.write()).await?;
    assert_eq!(harness.runtime.event_tx.latest_sequence(), sequence);
    assert!(harness.runtime.active_streams.lock().await.is_empty());
    assert_eq!(
        harness
            .runtime
            .agent_manager
            .read()
            .await
            .get_tip(&session)
            .await?,
        Some(request.message_id)
    );
    drop(state);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn cancel_turn_discards_prompt_queued_during_preparation() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime fixture");
    let session = create_session_with_profile(&harness.handle, "test-profile").await?;
    let mut events = harness.handle.subscribe();
    let preparation = harness.runtime.session_preparations.begin(&session).await;
    let submitted = harness
        .handle
        .send_message(
            session.clone(),
            "must not run".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    assert!(matches!(
        submitted,
        crate::SubmitMessageOutcome::Queued { .. }
    ));
    let handle = harness.handle.clone();
    let task_session = session.clone();
    let cancel = tokio::spawn(async move { handle.cancel_turn(task_session).await });
    tokio::time::timeout(TEST_TIMEOUT, async {
        while harness.runtime.session_state_barrier.try_write().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    drop(preparation);
    assert!(tokio::time::timeout(TEST_TIMEOUT, cancel).await???);
    harness
        .runtime
        .handle_start_queued_messages(session.clone())
        .await;
    let snapshot = harness.handle.get_session_snapshot(session).await?;
    assert_eq!(snapshot.queued_messages, 0);
    assert!(!snapshot.session.is_running);
    assert!(snapshot.history.is_empty());
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(event.event, Event::StreamStart { .. }));
    }
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn cancel_stream_preserves_queued_messages() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime fixture");
    let session = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .runtime
        .restore_queued_messages(
            &session,
            vec![crate::runtime::core::QueuedMessage {
                options: Default::default(),
                message: "keep queued".into(),
                model_id: kraai_types::ModelId::new("mock-model"),
                provider_id: kraai_types::ProviderId::new("mock"),
            }],
        )
        .await;
    assert!(!harness.handle.cancel_stream(session.clone()).await?);
    let queued = harness.runtime.take_queued_messages(&session).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued.first().expect("queued message").message.as_text(),
        Some("keep queued")
    );
    harness.shutdown().await;
    Ok(())
}
