use kraai_types::SessionId;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;
use futures::poll;
use tokio::sync::mpsc;

use super::harness::{RuntimeTestHarness, create_session_with_profile};
use crate::handle::Command;

#[tokio::test]
async fn failed_queue_preparation_restores_batch_and_allows_later_retry() -> Result<()> {
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
    tokio::time::timeout(
        Duration::from_secs(1),
        runtime.handle_start_queued_messages(session_id.clone()),
    )
    .await?;
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
    tokio::time::timeout(
        Duration::from_secs(1),
        runtime.handle_start_queued_messages(session_id.clone()),
    )
    .await?;
    assert!(
        super::harness::take_queue(runtime, &session_id)
            .await
            .is_empty()
    );
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn full_command_channel_and_queued_snapshot_do_not_block_terminal_drain() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let mut runtime = harness.runtime.clone();
    let (commands, mut receiver) = mpsc::channel(1);
    runtime.command_tx = commands;
    runtime.queue_drains = Arc::default();
    runtime
        .command_tx
        .try_send(Command::LoadConfig)
        .expect("fill command channel");
    let guard = runtime.session_state_barrier.read().await;
    let snapshot = runtime.build_session_snapshot(&session_id);
    tokio::pin!(snapshot);
    assert!(poll!(&mut snapshot).is_pending());

    super::harness::enqueue(
        &runtime,
        &session_id,
        vec![super::harness::queued(
            "queued".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("missing-provider"),
        )],
    )
    .await;

    // A failed continuation clears turn state and schedules a drain under the
    // caller's barrier. It must finish even though no command slot can free up.
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        runtime.start_continuation(
            session_id.clone(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("missing-provider"),
            Default::default(),
        ),
    )
    .await?;
    assert!(result.is_err());
    drop(guard);
    tokio::time::timeout(Duration::from_secs(1), snapshot).await??;

    runtime.schedule_queue_drain(&session_id);
    let expected_session = session_id.clone();
    let mut drain_received = false;
    let mut command_received = false;
    for _ in 0..2 {
        match tokio::time::timeout(Duration::from_secs(1), runtime.next_command(&mut receiver))
            .await?
        {
            Some(Command::StartQueuedMessages { session_id }) => {
                assert_eq!(session_id, expected_session);
                drain_received = true;
            }
            Some(Command::LoadConfig) => command_received = true,
            _ => panic!("unexpected command"),
        }
    }
    assert!(drain_received && command_received);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn drains_coalesce_without_losing_sessions_or_rescheduled_work() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime fixture");
    let mut runtime = harness.runtime.clone();
    runtime.queue_drains = Arc::default();
    let (_commands, mut receiver) = mpsc::channel(1);
    for session in ["first", "first", "second", "first", "second"] {
        runtime.schedule_queue_drain(&SessionId::new(session));
    }
    for expected in ["first", "second"] {
        let Some(Command::StartQueuedMessages { session_id }) =
            tokio::time::timeout(Duration::from_secs(1), runtime.next_command(&mut receiver))
                .await?
        else {
            panic!("missing drain");
        };
        assert_eq!(session_id, SessionId::new(expected));
    }
    {
        let next = runtime.next_command(&mut receiver);
        tokio::pin!(next);
        assert!(
            poll!(&mut next).is_pending(),
            "duplicate drain survived coalescing"
        );
    }
    runtime.schedule_queue_drain(&SessionId::new("first"));
    assert!(
        matches!(tokio::time::timeout(Duration::from_secs(1), runtime.next_command(&mut receiver)).await?, Some(Command::StartQueuedMessages { session_id }) if session_id == SessionId::new("first"))
    );
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn overlapping_preparations_preserve_queue_order_after_failure() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new()).await.expect("fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let mut runtime = harness.runtime.clone();
    runtime.queue_drains = Arc::default();
    let message = |text: &str| {
        super::harness::queued(
            text.into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
        )
    };
    super::harness::enqueue(&runtime, &session_id, vec![message("first")]).await;
    let first = runtime.start_continuation(
        session_id.clone(),
        kraai_types::ModelId::new("mock-model"),
        kraai_types::ProviderId::new("missing"),
        Default::default(),
    );
    tokio::pin!(first);
    assert!(poll!(&mut first).is_pending());
    assert_eq!(
        runtime
            .session_store
            .enqueue_message(&session_id, &message("second"))
            .await?,
        2
    );
    assert!(matches!(
        runtime
            .start_continuation(
                session_id.clone(),
                kraai_types::ModelId::new("mock-model"),
                kraai_types::ProviderId::new("mock"),
                Default::default(),
            )
            .await?,
        crate::ContinueSessionOutcome::NothingToContinue
    ));
    // Both entry points must coalesce before trying to acquire the agent lock.
    tokio::time::timeout(
        Duration::from_secs(1),
        runtime.handle_start_queued_messages(session_id.clone()),
    )
    .await?;
    assert!(first.await.is_err());
    assert!(!runtime.session_preparations.is_active(&session_id));
    let restored = super::harness::take_queue(&runtime, &session_id).await;
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
    let mut runtime = harness.runtime.clone();
    runtime.queue_drains = Arc::default();
    let preparation = runtime
        .session_preparations
        .try_begin(&session_id)
        .expect("first preparation");
    let send = runtime.handle_send_message(
        session_id.clone(),
        "new message".into(),
        kraai_types::ModelId::new("mock-model"),
        kraai_types::ProviderId::new("mock"),
        Default::default(),
    );
    tokio::pin!(send);
    assert!(poll!(&mut send).is_pending());
    // Preserve the decision to queue even if preparation finishes while this
    // submission is waiting to inspect turn state.
    let other = runtime
        .session_preparations
        .try_begin(&SessionId::new("another-session"))
        .expect("independent session");
    drop(other);
    drop(preparation);
    let outcome = send.await?;
    assert!(matches!(
        outcome,
        crate::SubmitMessageOutcome::Queued { position: 1 }
    ));
    assert!(runtime.agent_manager.get_tip(&session_id).await?.is_none());
    assert!(
        runtime
            .session_preparations
            .try_begin(&session_id)
            .is_some()
    );
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn preparation_starting_during_admission_queues_the_message() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new()).await.expect("fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let mut runtime = harness.runtime.clone();
    runtime.queue_drains = Arc::default();
    let send = runtime.handle_send_message(
        session_id.clone(),
        "new message".into(),
        kraai_types::ModelId::new("mock-model"),
        kraai_types::ProviderId::new("mock"),
        Default::default(),
    );
    tokio::pin!(send);
    assert!(poll!(&mut send).is_pending());
    let preparation = runtime
        .session_preparations
        .try_begin(&session_id)
        .expect("continuation preparation");
    assert!(matches!(
        send.await?,
        crate::SubmitMessageOutcome::Queued { position: 1 }
    ));
    let queued = super::harness::take_queue(&runtime, &session_id).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued.first().expect("queued message").content,
        "new message".into()
    );
    assert!(runtime.agent_manager.get_tip(&session_id).await?.is_none());
    drop(preparation);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn rejected_interception_preserves_turn_and_pending_workspace() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new()).await.expect("fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let mut runtime = harness.runtime.clone();
    runtime.queue_drains = Arc::default();
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
        &runtime,
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
            runtime.start_continuation(
                session_id.clone(),
                kraai_types::ModelId::new("mock-model"),
                kraai_types::ProviderId::new("missing"),
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
    let queued = super::harness::take_queue(&runtime, &session_id).await;
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
        .runtime
        .build_session_snapshot(&session_id)
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
