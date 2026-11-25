use super::super::*;
use super::common::{cleanup_dir, open_persistence, test_manager};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

struct ControlledSaves {
    inner: Arc<dyn MessageStore>,
    block: AtomicBool,
    fail: AtomicBool,
    release: Notify,
}

impl ControlledSaves {
    async fn gate(&self) -> Result<()> {
        if self.block.swap(false, Ordering::SeqCst) {
            self.release.notified().await;
        }
        if self.fail.swap(false, Ordering::SeqCst) {
            return Err(eyre!("injected snapshot save failure"));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl MessageStore for ControlledSaves {
    async fn save(&self, message: &Message) -> Result<()> {
        self.gate().await?;
        self.inner.save(message).await
    }
    async fn append_stream_deltas(
        &self,
        message_id: &MessageId,
        first_sequence: u64,
        deltas: &[kraai_persistence::StreamDelta],
    ) -> Result<()> {
        self.gate().await?;
        self.inner
            .append_stream_deltas(message_id, first_sequence, deltas)
            .await
    }
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        self.inner.get(id).await
    }
    async fn delete(&self, id: &MessageId) -> Result<()> {
        self.inner.delete(id).await
    }
    async fn exists(&self, id: &MessageId) -> Result<bool> {
        self.inner.exists(id).await
    }
    async fn list_ids(&self) -> Result<HashSet<MessageId>> {
        self.inner.list_ids().await
    }
}

#[tokio::test]
async fn slow_snapshots_allow_chunks_and_precede_terminal_saves() {
    for terminal in ["complete", "abort", "cancel"] {
        let (mut manager, data_dir) = test_manager().await;
        let session = manager.create_session().await.unwrap();
        let request = manager
            .prepare_start_stream(
                &session,
                "start".into(),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
                Default::default(),
            )
            .await
            .unwrap();
        manager
            .append_text_chunk(&request.message_id, "text", AssistantPhase::FinalAnswer, "")
            .await;
        let store = Arc::new(ControlledSaves {
            inner: manager.message_store.clone(),
            block: AtomicBool::new(true),
            fail: AtomicBool::new(false),
            release: Notify::new(),
        });
        manager.message_store = store.clone();
        let publication = manager.publish_streaming_snapshots();
        tokio::pin!(publication);
        assert!(futures::poll!(&mut publication).is_pending());
        let chunk = kraai_test_support::hang_guard(manager.append_text_chunk(
            &request.message_id,
            "text",
            AssistantPhase::FinalAnswer,
            "new chunk",
        ))
        .await
        .unwrap();
        assert_eq!(
            super::common::streamed_text(chunk).as_deref(),
            Some("new chunk")
        );
        let finishing = async {
            match terminal {
                "complete" => {
                    manager.complete_message(&request.message_id).await?;
                }
                "abort" => {
                    manager.abort_streaming_message(&request.message_id).await?;
                }
                _ => {
                    manager
                        .cancel_streaming_message(&request.message_id)
                        .await?;
                }
            }
            Ok::<_, color_eyre::Report>(())
        };
        tokio::pin!(finishing);
        assert!(futures::poll!(&mut finishing).is_pending());
        store.release.notify_one();
        assert!(publication.await.is_empty());
        assert!(
            !manager.streaming_messages.read().await[&request.message_id]
                .pending_deltas
                .is_empty()
        );
        finishing.await.unwrap();
        let saved = store.get(&request.message_id).await.unwrap();
        if terminal == "abort" {
            assert!(saved.is_none());
        } else {
            let saved = saved.expect("terminal message");
            let expected = if terminal == "complete" {
                MessageStatus::Complete
            } else {
                MessageStatus::Cancelled
            };
            assert_eq!(saved.status, expected);
            assert_eq!(saved.display_text(), "new chunk");
        }
        cleanup_dir(data_dir).await;
    }
}

#[tokio::test]
async fn failed_or_cancelled_snapshot_saves_remain_retryable() {
    let (mut manager, data_dir) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let request = manager
        .prepare_start_stream(
            &session,
            "start".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .append_text_chunk(
            &request.message_id,
            "text",
            AssistantPhase::FinalAnswer,
            "start",
        )
        .await;
    let store = Arc::new(ControlledSaves {
        inner: manager.message_store.clone(),
        block: AtomicBool::new(true),
        fail: AtomicBool::new(true),
        release: Notify::new(),
    });
    manager.message_store = store.clone();
    {
        let publication = manager.publish_streaming_snapshots();
        tokio::pin!(publication);
        assert!(futures::poll!(&mut publication).is_pending());
    }
    assert_eq!(manager.publish_streaming_snapshots().await.len(), 1);
    assert!(
        !manager.streaming_messages.read().await[&request.message_id]
            .pending_deltas
            .is_empty()
    );
    assert!(manager.publish_streaming_snapshots().await.is_empty());
    assert!(
        manager.streaming_messages.read().await[&request.message_id]
            .pending_deltas
            .is_empty()
    );
    manager
        .abort_streaming_message(&request.message_id)
        .await
        .unwrap();
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn usage_only_updates_reach_observers_and_reopened_storage() {
    let (manager, data_dir) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let request = manager
        .prepare_start_stream(
            &session,
            "start".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .append_text_chunk(
            &request.message_id,
            "text",
            AssistantPhase::FinalAnswer,
            "response",
        )
        .await;
    assert!(manager.publish_streaming_snapshots().await.is_empty());
    let observer = AgentManager::new(
        manager.cloned_provider_manager(),
        data_dir.clone(),
        open_persistence(&data_dir).await,
        data_dir.clone(),
    );
    let usage = TokenUsage {
        input_tokens: 100,
        output_tokens: 20,
        total_tokens: 120,
        ..Default::default()
    };
    manager
        .set_streaming_message_usage(&request.message_id, usage.clone())
        .await
        .unwrap();
    let before = observer
        .capture_session_snapshot(&session)
        .await
        .unwrap()
        .load()
        .await
        .unwrap();
    assert_eq!(
        before.history[&request.message_id]
            .generation
            .as_ref()
            .and_then(|generation| generation.usage.as_ref()),
        None,
    );
    assert_eq!(
        before.requests[&request.message_id].usage,
        Some(usage.clone())
    );
    assert!(before.context_usage.is_none());

    assert!(manager.publish_streaming_snapshots().await.is_empty());
    let published = observer
        .capture_session_snapshot(&session)
        .await
        .unwrap()
        .load()
        .await
        .unwrap();
    let reopened = open_persistence(&data_dir).await;
    let reopened = reopened
        .messages()
        .read_conversation(&session)
        .await
        .unwrap()
        .expect("reopened conversation missing");
    for history in [&published.history, &reopened.history] {
        let message = &history[&request.message_id];
        assert_eq!(
            message
                .generation
                .as_ref()
                .and_then(|generation| generation.usage.as_ref()),
            Some(&usage),
        );
        assert!(matches!(message.status, MessageStatus::Streaming { .. }));
        assert_eq!(message.display_text(), "response");
    }
    assert!(published.context_usage.is_none());
    assert_eq!(published.requests, before.requests);
    assert_eq!(reopened.requests, before.requests);

    manager.complete_message(&request.message_id).await.unwrap();
    let completed = observer
        .capture_session_snapshot(&session)
        .await
        .unwrap()
        .load()
        .await
        .unwrap();
    assert_eq!(
        completed.history[&request.message_id].status,
        MessageStatus::Complete
    );
    assert_eq!(
        completed.context_usage.map(|context| context.usage),
        Some(usage)
    );
    drop((manager, observer));
    cleanup_dir(data_dir).await;
}

struct FailingDeltas {
    inner: Arc<dyn MessageStore>,
    failing: MessageId,
}

#[async_trait::async_trait]
impl MessageStore for FailingDeltas {
    async fn save(&self, message: &Message) -> Result<()> {
        self.inner.save(message).await
    }
    async fn append_stream_deltas(
        &self,
        message_id: &MessageId,
        first_sequence: u64,
        deltas: &[kraai_persistence::StreamDelta],
    ) -> Result<()> {
        if *message_id == self.failing {
            return Err(eyre!("injected delta failure"));
        }
        self.inner
            .append_stream_deltas(message_id, first_sequence, deltas)
            .await
    }
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        self.inner.get(id).await
    }
    async fn delete(&self, id: &MessageId) -> Result<()> {
        self.inner.delete(id).await
    }
    async fn exists(&self, id: &MessageId) -> Result<bool> {
        self.inner.exists(id).await
    }
    async fn list_ids(&self) -> Result<HashSet<MessageId>> {
        self.inner.list_ids().await
    }
}

#[tokio::test]
async fn a_failing_session_does_not_stop_other_sessions_from_persisting() {
    let (mut manager, data_dir) = test_manager().await;
    let mut requests = Vec::new();
    for _ in 0..2 {
        let session = manager.create_session().await.unwrap();
        let request = manager
            .prepare_start_stream(
                &session,
                "start".into(),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
                Default::default(),
            )
            .await
            .unwrap();
        requests.push((session, request.message_id));
    }
    let [(failing_session, failing), (healthy_session, healthy)] = requests.as_slice() else {
        unreachable!();
    };
    manager.message_store = Arc::new(FailingDeltas {
        inner: manager.message_store.clone(),
        failing: failing.clone(),
    });
    for round in 0..2 {
        for (_, message) in &requests {
            manager
                .append_text_chunk(message, "text", AssistantPhase::FinalAnswer, "chunk")
                .await;
        }
        let failures: Vec<_> = manager
            .publish_streaming_snapshots()
            .await
            .into_iter()
            .map(|(session, _)| session)
            .collect();
        let expected = if round == 0 {
            vec![failing_session.clone()]
        } else {
            Vec::new()
        };
        assert_eq!(failures, expected);
        let streaming = manager.streaming_messages.read().await;
        assert!(streaming[healthy].pending_deltas.is_empty());
        assert!(!streaming[failing].pending_deltas.is_empty());
        drop(streaming);
    }
    let reopened = open_persistence(&data_dir).await;
    let saved = reopened
        .messages()
        .read_conversation(healthy_session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.history[healthy].display_text(), "chunkchunk");
    cleanup_dir(data_dir).await;
}
