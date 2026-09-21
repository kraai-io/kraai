mod append;
mod idempotent;
mod rollback;

use super::*;
use crate::{FileMessageStore, FileSessionStore, SessionMeta};
use kraai_types::{AssistantItem, AssistantPhase, ChatRole, ToolCallId};
use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Barrier, Notify};

fn test_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "agent-persistence-{name}-{nanos}-{}",
        Ulid::generate()
    ))
}

async fn with_test_store<T, F, Fut>(name: &str, f: F) -> T
where
    F: FnOnce(Arc<FileMessageStore>, Arc<FileSessionStore>, PathBuf) -> Fut,
    Fut: Future<Output = T>,
{
    let data_dir = test_dir(name);
    tokio::fs::create_dir_all(&data_dir).await.unwrap();
    let message_store = Arc::new(FileMessageStore::new(&data_dir));
    let session_store = Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));
    let result = f(message_store, session_store, data_dir.clone()).await;
    let _ = tokio::fs::remove_dir_all(&data_dir).await;
    result
}

fn untitled_session(id: &str, tip_id: Option<&MessageId>, updated_at: u64) -> SessionMeta {
    SessionMeta {
        id: id.to_string(),
        tip_id: tip_id.cloned(),
        workspace_dir: PathBuf::from("/tmp/workspace"),
        created_at: updated_at.saturating_sub(1),
        updated_at,
        title: None,
        selected_profile_id: None,
    }
}

struct FailOnSaveSessionStore {
    inner: Arc<dyn SessionStore>,
    should_fail: Arc<AtomicBool>,
    commit_before_failure: bool,
}

struct FailOnDeleteMessageStore {
    inner: Arc<dyn MessageStore>,
    should_fail: Arc<AtomicBool>,
}

struct BarrierOnSaveMessageStore {
    inner: Arc<dyn MessageStore>,
    barrier: Arc<Barrier>,
}

struct PauseBeforeFirstSaveMessageStore {
    inner: Arc<dyn MessageStore>,
    pause_next_save: AtomicBool,
    entered: Arc<Notify>,
    resume: Arc<Notify>,
}

struct PauseBeforeDeleteMessageStore {
    inner: Arc<dyn MessageStore>,
    entered: Arc<Notify>,
    resume: Arc<Notify>,
}

struct PauseAfterGetMessageStore {
    inner: Arc<dyn MessageStore>,
    pause_next_get: AtomicBool,
    entered: Arc<Notify>,
    resume: Arc<Notify>,
}

#[async_trait::async_trait]
impl MessageStore for PauseAfterGetMessageStore {
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        let message = self.inner.get(id).await?;
        if self.pause_next_get.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.resume.notified().await;
        }
        Ok(message)
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

#[async_trait::async_trait]
impl MessageStore for PauseBeforeDeleteMessageStore {
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        self.inner.get(id).await
    }

    async fn save(&self, message: &Message) -> Result<()> {
        self.inner.save(message).await
    }

    async fn unload(&self, id: &MessageId) {
        self.inner.unload(id).await;
    }

    async fn delete(&self, id: &MessageId) -> Result<()> {
        self.entered.notify_one();
        self.resume.notified().await;
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

#[async_trait::async_trait]
impl MessageStore for PauseBeforeFirstSaveMessageStore {
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        self.inner.get(id).await
    }

    async fn save(&self, message: &Message) -> Result<()> {
        if self.pause_next_save.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.resume.notified().await;
        }
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

#[async_trait::async_trait]
impl SessionStore for FailOnSaveSessionStore {
    async fn link_message_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
    ) -> Result<bool> {
        if self.should_fail.load(Ordering::SeqCst) {
            if self.commit_before_failure {
                self.inner
                    .link_message_if_tip_matches(session, expected_tip)
                    .await?;
            }
            return Err(eyre!("intentional session save failure for {}", session.id));
        }
        self.inner
            .link_message_if_tip_matches(session, expected_tip)
            .await
    }

    async fn lock_message_mutation(&self, id: &MessageId) -> tokio::sync::OwnedMutexGuard<()> {
        self.inner.lock_message_mutation(id).await
    }

    async fn delete_message_if_unreferenced(
        &self,
        id: &MessageId,
        message_store: Arc<dyn MessageStore>,
    ) -> Result<()> {
        self.inner
            .delete_message_if_unreferenced(id, message_store)
            .await
    }

    async fn list(&self) -> Result<Vec<SessionMeta>> {
        self.inner.list().await
    }

    async fn get(&self, id: &str) -> Result<Option<SessionMeta>> {
        self.inner.get(id).await
    }

    async fn save(&self, session: &SessionMeta) -> Result<()> {
        if self.should_fail.load(Ordering::SeqCst) {
            if self.commit_before_failure {
                self.inner.save(session).await?;
            }
            return Err(eyre!("intentional session save failure for {}", session.id));
        }
        self.inner.save(session).await
    }

    async fn save_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
    ) -> Result<bool> {
        if self.should_fail.load(Ordering::SeqCst) {
            if self.commit_before_failure {
                self.inner
                    .save_if_tip_matches(session, expected_tip)
                    .await?;
            }
            return Err(eyre!("intentional session save failure for {}", session.id));
        }
        self.inner.save_if_tip_matches(session, expected_tip).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await
    }
}

#[async_trait::async_trait]
impl MessageStore for FailOnDeleteMessageStore {
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        self.inner.get(id).await
    }

    async fn save(&self, message: &Message) -> Result<()> {
        self.inner.save(message).await
    }

    async fn unload(&self, id: &MessageId) {
        self.inner.unload(id).await;
    }

    async fn delete(&self, id: &MessageId) -> Result<()> {
        if self.should_fail.load(Ordering::SeqCst) {
            return Err(eyre!("intentional message delete failure for {id}"));
        }
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

#[async_trait::async_trait]
impl MessageStore for BarrierOnSaveMessageStore {
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        self.inner.get(id).await
    }

    async fn save(&self, message: &Message) -> Result<()> {
        self.inner.save(message).await?;
        self.barrier.wait().await;
        Ok(())
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

fn append_request(
    session_id: &str,
    role: ChatRole,
    content: &str,
    status: MessageStatus,
    title_if_first_message: Option<&str>,
) -> AppendMessageRequest {
    let content = match role {
        ChatRole::System => ConversationItem::System {
            text: content.to_string(),
        },
        ChatRole::User => ConversationItem::User {
            text: content.to_string(),
        },
        ChatRole::Assistant => ConversationItem::Assistant {
            items: if content.is_empty() {
                Vec::new()
            } else {
                vec![AssistantItem::Text {
                    phase: AssistantPhase::FinalAnswer,
                    text: content.to_string(),
                }]
            },
        },
        ChatRole::ToolCallResult => ConversationItem::ScriptResult {
            call_id: ToolCallId::new("test-call"),
            output: content.to_string(),
        },
    };
    AppendMessageRequest {
        session_id: session_id.to_string(),
        content,
        status,
        agent_profile_id: None,
        generation: None,
        title_if_first_message: title_if_first_message.map(str::to_string),
    }
}
