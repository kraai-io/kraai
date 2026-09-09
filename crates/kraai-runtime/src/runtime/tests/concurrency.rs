use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use color_eyre::eyre::Result;
use futures::poll;
use kraai_persistence::{FileMessageStore, MessageStore};
use kraai_types::{Message, MessageId};
use tokio_util::sync::CancellationToken;

use super::harness::{RuntimeTestHarness, create_session_with_profile};
use crate::runtime::core::ActiveScriptTask;
use crate::{Event, SessionActivity};

#[tokio::test]
async fn stream_start_events_precede_a_snapshot_queued_during_preparation() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let mutation = harness.runtime.session_state_barrier.read().await;
    let mut agent = harness.runtime.agent_manager.write().await;
    let mut request = agent
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
        )
        .await?;
    let providers = agent.cloned_provider_manager();
    drop(agent);
    request.context_notifications = vec!["missing file automatically unpinned".into()];
    let message_id = request.message_id.to_string();
    let mut events = harness.runtime.event_tx.subscribe();
    let runtime = harness.runtime.clone();
    let snapshot = runtime.build_session_snapshot(&session_id);
    tokio::pin!(snapshot);
    assert!(poll!(&mut snapshot).is_pending());

    harness
        .runtime
        .start_stream_job(
            super::super::streaming::StreamJobKind::Initial,
            session_id.clone(),
            providers,
            request,
        )
        .await;
    // Neither the spawned provider task nor a snapshot may separate registration
    // from its initial events, even when preparation produced notifications.
    let context = events.try_recv()?;
    assert!(
        matches!(context.event, Event::ContextStateChanged { notifications, .. } if notifications.len() == 1)
    );
    let start = events.try_recv()?;
    assert!(
        matches!(start.event, Event::StreamStart { message_id: actual, .. } if actual == message_id)
    );
    assert!(context.sequence < start.sequence);
    drop(mutation);
    let snapshot = tokio::time::timeout(Duration::from_secs(1), snapshot).await??;
    assert!(snapshot.event_sequence >= start.sequence);
    assert_eq!(snapshot.activity, SessionActivity::Streaming);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn cancellation_finishes_with_a_queued_snapshot_and_stays_active_until_finalized()
-> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let cancellation = CancellationToken::new();
    let completion = CancellationToken::new();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let runtime = harness.runtime.clone();
    let task_session = session_id.clone();
    let completion_guard = completion.clone().drop_guard();
    let join_handle = tokio::spawn(async move {
        let _completion_guard = completion_guard;
        finish_rx.await.expect("release finalization");
        let _guard = runtime.session_state_barrier.read().await;
        runtime
            .active_script_tasks
            .lock()
            .await
            .remove(&task_session);
        runtime.send_event(Event::HistoryUpdated {
            session_id: task_session,
        });
    });
    harness.runtime.active_script_tasks.lock().await.insert(
        session_id.clone(),
        ActiveScriptTask {
            cancellation: cancellation.clone(),
            completion: completion.clone(),
            join_handle,
        },
    );

    let (response, result) = tokio::sync::oneshot::channel();
    let cancel_runtime = harness.runtime.clone();
    let cancel = cancel_runtime.handle_command(crate::handle::Command::CancelStream {
        session_id: session_id.clone(),
        response,
    });
    tokio::pin!(cancel);
    assert!(poll!(&mut cancel).is_pending());
    assert!(cancellation.is_cancelled());

    // Force the snapshot writer to queue before the completion path requests a reader.
    let mutation = harness.runtime.session_state_barrier.read().await;
    let snapshot = cancel_runtime.build_session_snapshot(&session_id);
    tokio::pin!(snapshot);
    assert!(poll!(&mut snapshot).is_pending());
    finish_tx.send(()).expect("finish task still running");
    tokio::task::yield_now().await;
    drop(mutation);
    let snapshot = tokio::time::timeout(Duration::from_secs(1), snapshot).await??;
    assert_eq!(snapshot.activity, SessionActivity::ExecutingScript);
    tokio::time::timeout(Duration::from_secs(1), &mut cancel).await??;
    assert!(result.await??);
    assert!(completion.is_cancelled());
    assert!(harness.runtime.active_script_tasks.lock().await.is_empty());
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn slow_snapshot_history_does_not_block_commands_or_state_events() -> Result<()> {
    let data_dir =
        std::env::temp_dir().join(format!("kraai-slow-history-{}", ulid::Ulid::generate()));
    let store = Arc::new(PausedMessageStore {
        inner: FileMessageStore::new(&data_dir),
        pause: AtomicBool::new(false),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let harness = RuntimeTestHarness::new_with_message_store(
        kraai_provider_core::ProviderManager::new(),
        Some(store.clone()),
    )
    .await
    .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let message_id = MessageId::new(ulid::Ulid::generate());
    harness
        .runtime
        .agent_manager
        .write()
        .await
        .add_script_result_to_history(
            &session_id,
            message_id.clone(),
            "test-profile".into(),
            kraai_types::ToolCallId::new("test-call"),
            "old history".into(),
        )
        .await?;
    store.pause.store(true, Ordering::SeqCst);
    let snapshot_task = tokio::spawn({
        let handle = harness.handle.clone();
        let session_id = session_id.clone();
        async move { handle.get_session_snapshot(session_id).await }
    });
    tokio::time::timeout(Duration::from_secs(1), store.entered.notified()).await?;
    tokio::time::timeout(
        Duration::from_secs(1),
        harness.handle.list_provider_definitions(),
    )
    .await??;
    let guard = tokio::time::timeout(
        Duration::from_secs(1),
        harness.runtime.session_state_barrier.read(),
    )
    .await?;
    harness
        .runtime
        .send_event(Event::HistoryUpdated { session_id });
    let later_sequence = harness.runtime.event_tx.latest_sequence();
    drop(guard);
    store.release.notify_one();
    let snapshot = tokio::time::timeout(Duration::from_secs(1), snapshot_task).await???;
    assert!(snapshot.event_sequence < later_sequence);
    assert_eq!(
        snapshot
            .history
            .get(&message_id)
            .expect("captured history")
            .content,
        kraai_types::ConversationItem::ScriptResult {
            call_id: kraai_types::ToolCallId::new("test-call"),
            output: "old history".into()
        }
    );
    harness.shutdown().await;
    tokio::fs::remove_dir_all(data_dir).await?;
    Ok(())
}

struct PausedMessageStore {
    inner: FileMessageStore,
    pause: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl MessageStore for PausedMessageStore {
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        if self.pause.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.get(id).await
    }
    async fn save(&self, message: &Message) -> Result<()> {
        self.inner.save(message).await
    }
    async fn unload(&self, id: &MessageId) {
        self.inner.unload(id).await;
    }
    async fn delete(&self, id: &MessageId) -> Result<()> {
        self.inner.delete(id).await
    }
    async fn exists(&self, id: &MessageId) -> Result<bool> {
        self.inner.exists(id).await
    }
    async fn list_all_on_disk(&self) -> Result<HashSet<MessageId>> {
        self.inner.list_all_on_disk().await
    }
    async fn list_hot(&self) -> Result<HashSet<MessageId>> {
        self.inner.list_hot().await
    }
}
