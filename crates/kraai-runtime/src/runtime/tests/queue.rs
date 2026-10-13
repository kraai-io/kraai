use std::time::Duration;

use color_eyre::eyre::Result;

use super::harness::FullHistory as _;
use super::harness::{RuntimeTestHarness, TEST_TIMEOUT, create_session_with_profile};
use crate::Event;

async fn drain(harness: &RuntimeTestHarness, session_id: &kraai_types::SessionId) {
    harness
        .runtime
        .with_session_actor(session_id, |actor| {
            Box::pin(async move { actor.drain().await })
        })
        .await;
}

#[tokio::test]
async fn failed_queue_preparation_keeps_the_batch_and_allows_later_retry() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let runtime = &harness.runtime;
    super::harness::enqueue(
        runtime,
        &session_id,
        ["one", "two"].into_iter().map(|message| {
            super::harness::queued(
                message.into(),
                kraai_types::ModelId::new("mock-model"),
                kraai_types::ProviderId::new("missing-provider"),
            )
        }),
    )
    .await;
    drain(&harness, &session_id).await;
    harness
        .events
        .wait_for("queued preparation failure", |events| {
            events.iter().any(|event| {
                matches!(event, Event::SessionError { session_id: id, .. } if id == &session_id)
            })
        })
        .await;
    assert!(!runtime.agent_manager.is_turn_active(&session_id));
    let mut messages = super::harness::take_queue(runtime, &session_id).await;
    assert_eq!(
        messages
            .iter()
            .map(|message| message.content.as_text().unwrap_or(""))
            .collect::<Vec<_>>(),
        vec!["one", "two"]
    );
    assert!(runtime.agent_manager.get_tip(&session_id).await?.is_none());
    for message in &mut messages {
        message.selection.provider_id = kraai_types::ProviderId::new("mock");
    }
    super::harness::enqueue(runtime, &session_id, messages).await;
    drain(&harness, &session_id).await;
    harness
        .events
        .wait_for("queued stream", |events| {
            events.iter().any(|event| {
                matches!(event, Event::StreamStart { session_id: id, .. } if id == &session_id)
            })
        })
        .await;
    assert!(super::harness::queue(runtime, &session_id).await.is_empty());
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn failed_continuation_preserves_queue_order_with_messages_queued_meanwhile() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new()).await.expect("fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let message = |text: &str| {
        super::harness::queued(
            text.into(),
            kraai_types::ModelId::new("missing-model"),
            kraai_types::ProviderId::new("mock"),
        )
    };
    super::harness::enqueue(&harness.runtime, &session_id, vec![message("first")]).await;
    let second = message("second");
    let (failed, overlapping) = harness
        .runtime
        .with_session_actor(&session_id, move |actor| {
            Box::pin(async move {
                let (failed_response, failed) = tokio::sync::oneshot::channel();
                actor.continue_session(None, Some(failed_response)).await;
                actor
                    .runtime
                    .session_store
                    .enqueue_message(&actor.session_id, &second)
                    .await
                    .expect("enqueue during preparation");
                let (overlapping_response, overlapping) = tokio::sync::oneshot::channel();
                actor
                    .continue_session(None, Some(overlapping_response))
                    .await;
                (failed, overlapping)
            })
        })
        .await;
    assert_eq!(
        tokio::time::timeout(TEST_TIMEOUT, overlapping).await???,
        crate::ContinueSessionOutcome::NothingToContinue
    );
    assert!(tokio::time::timeout(TEST_TIMEOUT, failed).await??.is_err());
    let restored = super::harness::take_queue(&harness.runtime, &session_id).await;
    assert_eq!(
        restored
            .iter()
            .map(|m| m.content.as_text().unwrap_or(""))
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn messages_arriving_during_preparation_stay_queued() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new()).await.expect("fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    super::harness::hold_preparation(&harness.runtime, &session_id).await;
    let outcome = harness
        .handle
        .send_message(
            session_id.clone(),
            "new message".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    assert!(matches!(
        outcome,
        crate::SubmitMessageOutcome::Queued { position: 1 }
    ));
    assert!(
        harness
            .runtime
            .agent_manager
            .get_tip(&session_id)
            .await?
            .is_none()
    );
    let queued = super::harness::queue(&harness.runtime, &session_id).await;
    assert_eq!(
        queued.first().map(|message| &message.content),
        Some(&"new message".into())
    );
    super::harness::release_preparation(&harness.runtime, &session_id).await;
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn rejected_continuation_preserves_turn_and_pending_workspace() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new()).await.expect("fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let runtime = &harness.runtime;
    let (tip, workspace) = {
        let agent = &runtime.agent_manager;
        let request = agent
            .prepare_start_stream(
                &session_id,
                "first".into(),
                kraai_types::ModelId::new("mock-model"),
                kraai_types::ProviderId::new("mock"),
                Default::default(),
            )
            .await?;
        agent.complete_message(&request.message_id).await?;
        let turn = agent.script_turn_context(&session_id)?;
        let pending_workspace = turn.workspace_dir.join("pending");
        tokio::fs::create_dir_all(pending_workspace.join(".kraai")).await?;
        tokio::fs::copy(
            turn.workspace_dir.join(".kraai/agents.toml"),
            pending_workspace.join(".kraai/agents.toml"),
        )
        .await?;
        agent
            .set_workspace_dir(&session_id, pending_workspace)
            .await?;
        (request.message_id, turn.workspace_dir.join("pending"))
    };
    super::harness::enqueue(
        runtime,
        &session_id,
        vec![super::harness::queued(
            "queued".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("missing"),
        )],
    )
    .await;
    assert!(
        tokio::time::timeout(
            Duration::from_secs(1),
            harness.handle.continue_session(
                session_id.clone(),
                "mock-model".into(),
                "missing".into(),
                Default::default(),
            ),
        )
        .await?
        .is_err()
    );
    assert!(runtime.agent_manager.is_turn_active(&session_id));
    assert!(runtime.session_store.owns_turn(&session_id).await?);
    assert_eq!(runtime.agent_manager.get_tip(&session_id).await?, Some(tip));
    assert_eq!(
        runtime
            .agent_manager
            .get_workspace_dir_state(&session_id)
            .await?,
        Some((workspace, true))
    );
    let queued = super::harness::take_queue(runtime, &session_id).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued.first().expect("restored message").content,
        "queued".into()
    );
    harness.shutdown().await;
    Ok(())
}
#[tokio::test]
async fn queued_messages_survive_restart_and_drain_on_the_next_send() -> Result<()> {
    let previous = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("previous runtime");
    let session_id = create_session_with_profile(&previous.handle, "test-profile").await?;
    super::harness::enqueue(
        &previous.runtime,
        &session_id,
        [super::harness::queued(
            "queued before restart".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
        )],
    )
    .await;
    let restarted = RuntimeTestHarness::new_shared(
        vec![vec![super::harness::ScriptedChunk::plain("both handled")]],
        previous.data_dir.clone(),
    )
    .await
    .expect("restarted runtime");
    let snapshot = restarted
        .handle
        .get_session_snapshot(session_id.clone())
        .await?;
    assert_eq!(snapshot.queued_messages, 1);
    assert!(!snapshot.session.is_running);
    assert!(snapshot.history.is_empty());

    assert_eq!(
        restarted
            .handle
            .send_message(
                session_id.clone(),
                "sent after restart".into(),
                "mock-model".into(),
                "mock".into(),
                Default::default(),
            )
            .await?,
        crate::SubmitMessageOutcome::Queued { position: 2 }
    );
    restarted
        .events
        .wait_for("queued turn", |events| {
            events.iter().any(|event| {
                matches!(event, crate::Event::TurnCompleted { session_id: id } if id == &session_id)
            })
        })
        .await;
    let users: Vec<_> = restarted
        .handle
        .get_chat_history(session_id.clone())
        .await?
        .into_values()
        .filter_map(|message| match message.content {
            kraai_types::ConversationItem::User { content } => {
                content.as_text().map(str::to_string)
            }
            _ => None,
        })
        .collect();
    assert_eq!(users.len(), 2);
    assert!(users.contains(&"queued before restart".to_string()));
    assert!(users.contains(&"sent after restart".to_string()));
    assert!(
        super::harness::queue(&restarted.runtime, &session_id)
            .await
            .is_empty()
    );
    restarted.shutdown().await;
    previous.shutdown().await;
    Ok(())
}
