use super::super::*;
use super::common::{cleanup_dir, test_manager};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

struct ControlledSaves {
    inner: Arc<dyn MessageStore>,
    block: AtomicBool,
    fail: AtomicBool,
    release: Notify,
}

#[async_trait::async_trait]
impl MessageStore for ControlledSaves {
    async fn save(&self, message: &Message) -> Result<()> {
        if self.block.swap(false, Ordering::SeqCst) {
            self.release.notified().await;
        }
        if self.fail.swap(false, Ordering::SeqCst) {
            return Err(eyre!("injected snapshot save failure"));
        }
        self.inner.save(message).await
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
async fn slow_snapshots_allow_chunks_and_precede_terminal_saves() -> Result<()> {
    for terminal in ["complete", "abort", "cancel"] {
        let (mut manager, data_dir) = test_manager().await;
        let session = manager.create_session().await?;
        let request = manager
            .prepare_start_stream(
                &session,
                "start".into(),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await?;
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
        let chunk = tokio::time::timeout(
            Duration::from_secs(15),
            manager.append_text_chunk(
                &request.message_id,
                "text",
                AssistantPhase::FinalAnswer,
                "new chunk",
            ),
        )
        .await?;
        assert_eq!(chunk.as_deref(), Some("new chunk"));
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
                        .cancel_streaming_message(&request.message_id, "cancelled")
                        .await?;
                }
            }
            Ok::<_, color_eyre::Report>(())
        };
        tokio::pin!(finishing);
        assert!(futures::poll!(&mut finishing).is_pending());
        store.release.notify_one();
        publication.await?;
        assert!(manager.streaming_messages.read().await[&request.message_id].snapshot_dirty);
        finishing.await?;
        let saved = store.get(&request.message_id).await?;
        if terminal == "abort" {
            assert!(saved.is_none());
        } else {
            let saved = saved.expect("terminal message");
            assert_eq!(saved.status, MessageStatus::Complete);
            assert_eq!(saved.display_text(), "new chunk");
        }
        cleanup_dir(data_dir).await;
    }
    Ok(())
}

#[tokio::test]
async fn failed_or_cancelled_snapshot_saves_remain_retryable() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session = manager.create_session().await?;
    let request = manager
        .prepare_start_stream(
            &session,
            "start".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
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
    assert!(manager.publish_streaming_snapshots().await.is_err());
    assert!(manager.streaming_messages.read().await[&request.message_id].snapshot_dirty);
    manager.publish_streaming_snapshots().await?;
    assert!(!manager.streaming_messages.read().await[&request.message_id].snapshot_dirty);
    manager.abort_streaming_message(&request.message_id).await?;
    cleanup_dir(data_dir).await;
    Ok(())
}
