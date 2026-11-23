use kraai_execution_types::ScriptExecutionStatus;
use kraai_persistence::ScriptExecutionCompletion;
use kraai_test_support::hang_guard;
use kraai_types::SessionId;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;
use crate::runtime::script_execution::CompletedScriptExecution;

async fn finished_execution() -> (RuntimeTestHarness, SessionId, CompletedScriptExecution) {
    let harness = RuntimeTestHarness::new(vec![vec![ScriptedChunk::native_call(
        "cancel-call",
        "# timeout=30sec permissions=workspace-write\n'changed' | save result.txt",
    )]])
    .await;
    let session = create_session_with_profile(&harness.handle, "test-profile").await;
    harness
        .handle
        .send_message(
            session.clone(),
            "change it".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await
        .unwrap();
    harness.events.wait_for("script approval", |events| {
        events.iter().any(|event| matches!(event, Event::ScriptApprovalRequested { session_id, .. } if session_id == &session))
    }).await;
    let pending = super::harness::take_pending_approval(&harness.runtime, &session).await;
    harness
        .runtime
        .execution_store
        .mark_running(&pending.request.id)
        .await
        .unwrap();
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
        .await
        .unwrap();
    let output = harness
        .runtime
        .execution_store
        .read_output(&record.id)
        .await
        .unwrap();
    (
        harness,
        session,
        CompletedScriptExecution { record, output },
    )
}

#[tokio::test]
async fn cancellation_after_execution_finishes_waits_for_result_and_prevents_continuation() {
    let (harness, session, completed) = finished_execution().await;
    let runtime = harness.runtime.clone();
    let (generation, cancellation) = runtime
        .with_session_actor(&session, |actor| {
            Box::pin(async move { actor.begin_test_script() })
        })
        .await;
    let handle = harness.handle.clone();
    let cancel_session = session.clone();
    let cancel = tokio::spawn(async move { handle.cancel_stream(cancel_session).await });
    hang_guard(cancellation.cancelled()).await.unwrap();
    assert!(!runtime.agent_manager.is_turn_active(&session));
    let observer = kraai_persistence::Persistence::open(&harness.data_dir)
        .await
        .unwrap();
    assert!(observer.sessions().claim_turn(&session).await.is_err());
    assert!(!cancel.is_finished());
    let stream_starts = || {
        harness
            .events
            .snapshot()
            .iter()
            .filter(|event| matches!(event, Event::StreamStart { session_id, .. } if session_id == &session))
            .count()
    };
    let started = stream_starts();
    runtime.sessions.send(
        &runtime,
        &session,
        crate::runtime::session::SessionMessage::ScriptEnded {
            generation,
            result: Ok(completed),
        },
    );
    assert!(hang_guard(cancel).await.unwrap().unwrap().unwrap());
    assert!(!runtime.session_store.owns_turn(&session).await.unwrap());
    let snapshot = harness
        .handle
        .get_session_snapshot(session.clone())
        .await
        .unwrap();
    assert!(!snapshot.session.is_running);
    assert!(snapshot.history.values().any(|message| matches!(
        message.content,
        kraai_types::ConversationItem::ScriptResult { .. }
    )));
    assert_eq!(stream_starts(), started);
    harness.shutdown().await;
}

#[tokio::test]
async fn cancellation_during_continuation_preparation_cancels_the_started_stream() {
    let (harness, session, completed) = finished_execution().await;
    harness
        .runtime
        .agent_manager
        .add_script_result_to_history(
            &session,
            completed.record.result_message_id.clone(),
            completed.record.profile.id.clone(),
            completed.record.call_id.clone(),
            harness
                .runtime
                .render_script_result(&completed)
                .await
                .unwrap(),
            completed.record.outcome().unwrap(),
        )
        .await
        .unwrap();
    let (response, cancelled) = tokio::sync::oneshot::channel();
    harness
        .runtime
        .with_session_actor(&session, move |actor| {
            Box::pin(async move {
                actor.continue_turn().await;
                assert_eq!(
                    actor.activity(),
                    crate::runtime::session::ActivityKind::Preparing
                );
                actor.cancel(false, None, Some(response)).await;
            })
        })
        .await;
    assert!(hang_guard(cancelled).await.unwrap().unwrap().unwrap());
    harness
        .events
        .wait_for("cancelled continuation", |events| {
            events.iter().any(|event| {
                matches!(event, Event::StreamCancelled { session_id, .. } if session_id == &session)
            })
        })
        .await;
    assert!(!harness.runtime.agent_manager.is_turn_active(&session));
    assert!(
        !harness
            .runtime
            .agent_manager
            .session_has_active_stream(&session)
            .await
    );
    harness.shutdown().await;
}
#[tokio::test]
async fn cancel_turn_discards_prompt_queued_during_preparation() {
    let harness = RuntimeTestHarness::new(Vec::new()).await;
    let session = create_session_with_profile(&harness.handle, "test-profile").await;
    let mut events = harness.handle.subscribe();
    super::harness::hold_preparation(&harness.runtime, &session).await;
    let submitted = harness
        .handle
        .send_message(
            session.clone(),
            "must not run".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await
        .unwrap();
    assert!(matches!(
        submitted,
        crate::SubmitMessageOutcome::Queued { .. }
    ));
    let handle = harness.handle.clone();
    let task_session = session.clone();
    let cancel = tokio::spawn(async move { handle.cancel_turn(task_session).await });
    tokio::task::yield_now().await;
    super::harness::release_preparation(&harness.runtime, &session).await;
    assert!(hang_guard(cancel).await.unwrap().unwrap().unwrap());
    let snapshot = harness.handle.get_session_snapshot(session).await.unwrap();
    assert_eq!(snapshot.queued_messages, 0);
    assert!(!snapshot.session.is_running);
    assert!(snapshot.history.is_empty());
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(event.event, Event::StreamStart { .. }));
    }
    harness.shutdown().await;
}

#[tokio::test]
async fn overlapping_cancellations_during_preparation_are_all_answered() {
    let harness = RuntimeTestHarness::new(Vec::new()).await;
    let session = create_session_with_profile(&harness.handle, "test-profile").await;
    super::harness::hold_preparation(&harness.runtime, &session).await;
    let cancellations: Vec<_> = (0..2)
        .map(|_| {
            let (response, answer) = tokio::sync::oneshot::channel();
            harness.runtime.sessions.send(
                &harness.runtime,
                &session,
                crate::runtime::session::SessionMessage::Cancel {
                    discard_queue: false,
                    expected_lease: None,
                    response: Some(response),
                },
            );
            answer
        })
        .collect();
    super::harness::release_preparation(&harness.runtime, &session).await;
    for answer in cancellations {
        assert!(
            !hang_guard(answer)
                .await
                .unwrap()
                .expect("every cancellation is answered")
                .unwrap()
        );
    }
    harness.shutdown().await;
}

#[tokio::test]
async fn cancel_stream_preserves_queued_messages() {
    let harness = RuntimeTestHarness::new(Vec::new()).await;
    let session = create_session_with_profile(&harness.handle, "test-profile").await;
    super::harness::enqueue(
        &harness.runtime,
        &session,
        vec![super::harness::queued(
            "keep queued".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
        )],
    )
    .await;
    assert!(!harness.handle.cancel_stream(session.clone()).await.unwrap());
    let queued = super::harness::take_queue(&harness.runtime, &session).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued.first().expect("queued message").content.as_text(),
        Some("keep queued")
    );
    harness.shutdown().await;
}
