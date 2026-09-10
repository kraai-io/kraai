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
    runtime
        .restore_queued_messages(
            &session_id,
            ["one", "two"]
                .into_iter()
                .map(|message| crate::runtime::core::QueuedMessage {
                    message: message.into(),
                    model_id: kraai_types::ModelId::new("mock-model"),
                    provider_id: kraai_types::ProviderId::new("missing-provider"),
                })
                .collect(),
        )
        .await;
    tokio::time::timeout(
        Duration::from_secs(1),
        runtime.handle_start_queued_messages(session_id.clone()),
    )
    .await?;
    assert!(
        !runtime
            .agent_manager
            .read()
            .await
            .is_turn_active(&session_id)
    );
    let mut messages = runtime.take_queued_messages(&session_id).await;
    assert_eq!(
        messages
            .iter()
            .map(|message| message.message.as_str())
            .collect::<Vec<_>>(),
        vec!["one", "two"]
    );
    assert!(
        runtime
            .agent_manager
            .read()
            .await
            .get_tip(&session_id)
            .await?
            .is_none()
    );
    for message in &mut messages {
        message.provider_id = kraai_types::ProviderId::new("mock");
    }
    runtime.restore_queued_messages(&session_id, messages).await;
    tokio::time::timeout(
        Duration::from_secs(1),
        runtime.handle_start_queued_messages(session_id.clone()),
    )
    .await?;
    assert!(runtime.take_queued_messages(&session_id).await.is_empty());
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

    // A failed continuation clears turn state and schedules a drain under the
    // caller's barrier. It must finish even though no command slot can free up.
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        runtime.start_continuation("missing-session".into()),
    )
    .await?;
    assert!(result.is_err());
    drop(guard);
    tokio::time::timeout(Duration::from_secs(1), snapshot).await??;

    let mut drain_received = false;
    let mut command_received = false;
    for _ in 0..2 {
        match tokio::time::timeout(Duration::from_secs(1), runtime.next_command(&mut receiver))
            .await?
        {
            Some(Command::StartQueuedMessages { session_id }) => {
                assert_eq!(session_id, "missing-session");
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
        runtime.schedule_queue_drain(session);
    }
    for expected in ["first", "second"] {
        let Some(Command::StartQueuedMessages { session_id }) =
            tokio::time::timeout(Duration::from_secs(1), runtime.next_command(&mut receiver))
                .await?
        else {
            panic!("missing drain");
        };
        assert_eq!(session_id, expected);
    }
    {
        let next = runtime.next_command(&mut receiver);
        tokio::pin!(next);
        assert!(
            poll!(&mut next).is_pending(),
            "duplicate drain survived coalescing"
        );
    }
    runtime.schedule_queue_drain("first");
    assert!(
        matches!(tokio::time::timeout(Duration::from_secs(1), runtime.next_command(&mut receiver)).await?, Some(Command::StartQueuedMessages { session_id }) if session_id == "first")
    );
    harness.shutdown().await;
    Ok(())
}
