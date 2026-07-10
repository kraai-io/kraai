use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use kraai_types::{
    ChatRole, Message, MessageGeneration, MessageId, MessageStatus, ToolStateDelta,
    ToolStateSnapshot,
};
use ulid::Ulid;

use crate::{MessageStore, SessionStore};

fn current_unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs()
}

#[derive(Clone)]
pub struct ConversationStore {
    message_store: Arc<dyn MessageStore>,
    session_store: Arc<dyn SessionStore>,
}

impl ConversationStore {
    pub fn new(message_store: Arc<dyn MessageStore>, session_store: Arc<dyn SessionStore>) -> Self {
        Self {
            message_store,
            session_store,
        }
    }

    pub async fn append_message(&self, request: AppendMessageRequest) -> Result<AppendedMessage> {
        let mut session = self
            .session_store
            .get(&request.session_id)
            .await?
            .ok_or_else(|| eyre!("Session not found: {}", request.session_id))?;
        let previous_tip = session.tip_id.clone();
        let previous_title = session.title.clone();
        let message_id = MessageId::new(Ulid::new());
        let message = Message {
            id: message_id.clone(),
            parent_id: previous_tip.clone(),
            role: request.role,
            content: request.content,
            status: request.status,
            agent_profile_id: request.agent_profile_id,
            tool_state_snapshot: request.tool_state_snapshot,
            tool_state_deltas: request.tool_state_deltas,
            generation: request.generation,
        };

        self.message_store.save(&message).await?;

        session.tip_id = Some(message_id.clone());
        if previous_tip.is_none()
            && session.title.is_none()
            && let Some(title) = request.title_if_first_message
        {
            session.title = Some(title);
        }
        session.updated_at = current_unix_timestamp();

        match self
            .session_store
            .save_if_tip_matches(&session, previous_tip.as_ref())
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                self.delete_unreferenced_message(&message_id).await;
                return Err(eyre!(
                    "Session {} changed while appending message {message_id}",
                    request.session_id
                ));
            }
            Err(error) => {
                self.delete_unreferenced_message(&message_id).await;
                return Err(error);
            }
        }

        Ok(AppendedMessage {
            message,
            previous_tip,
            previous_title,
        })
    }

    pub async fn restore_appended_message(
        &self,
        session_id: &str,
        appended: &AppendedMessage,
    ) -> Result<()> {
        self.restore_tip_title_and_delete_message(
            session_id,
            &appended.message.id,
            appended.previous_tip.clone(),
            appended.previous_title.clone(),
        )
        .await
    }

    pub async fn restore_tip_title_and_delete_message(
        &self,
        session_id: &str,
        message_id: &MessageId,
        tip_id: Option<MessageId>,
        title: Option<String>,
    ) -> Result<()> {
        let mut session = self
            .session_store
            .get(session_id)
            .await?
            .ok_or_else(|| eyre!("Session not found: {session_id}"))?;
        if session.tip_id.as_ref() != Some(message_id) {
            return Err(eyre!(
                "Cannot restore session {session_id}: tip is not abandoned message {message_id}"
            ));
        }

        session.tip_id = tip_id;
        session.title = title;
        session.updated_at = current_unix_timestamp();
        if !self
            .session_store
            .save_if_tip_matches(&session, Some(message_id))
            .await?
        {
            return Err(eyre!(
                "Cannot restore session {session_id}: tip changed while restoring message {message_id}"
            ));
        }
        // The session no longer references the abandoned message. Cleanup failure should leave an
        // orphan for later GC, not make callers believe the rollback itself failed.
        if let Err(error) = self.message_store.delete(message_id).await {
            tracing::error!(
                "Failed to delete abandoned message {message_id} after restoring session {session_id}: {error}"
            );
        }
        Ok(())
    }

    async fn delete_unreferenced_message(&self, message_id: &MessageId) {
        if let Err(delete_error) = self.message_store.delete(message_id).await {
            tracing::error!(
                "Failed to delete unreferenced appended message {message_id}: {delete_error}"
            );
        }
    }
}

pub struct AppendMessageRequest {
    pub session_id: String,
    pub role: ChatRole,
    pub content: String,
    pub status: MessageStatus,
    pub agent_profile_id: Option<String>,
    pub tool_state_snapshot: Option<ToolStateSnapshot>,
    pub tool_state_deltas: Vec<ToolStateDelta>,
    pub generation: Option<MessageGeneration>,
    pub title_if_first_message: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AppendedMessage {
    pub message: Message,
    pub previous_tip: Option<MessageId>,
    pub previous_title: Option<String>,
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "turn persistence tests use direct assertions for fixture and failure-path setup"
)]
mod tests {
    use super::*;
    use crate::{FileMessageStore, FileSessionStore, SessionMeta};
    use std::collections::HashSet;
    use std::future::Future;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::Barrier;

    fn test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("agent-persistence-{name}-{nanos}-{}", Ulid::new()))
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
    }

    struct FailOnDeleteMessageStore {
        inner: Arc<dyn MessageStore>,
        should_fail: Arc<AtomicBool>,
    }

    struct BarrierOnSaveMessageStore {
        inner: Arc<dyn MessageStore>,
        barrier: Arc<Barrier>,
    }

    #[async_trait::async_trait]
    impl SessionStore for FailOnSaveSessionStore {
        async fn list(&self) -> Result<Vec<SessionMeta>> {
            self.inner.list().await
        }

        async fn get(&self, id: &str) -> Result<Option<SessionMeta>> {
            self.inner.get(id).await
        }

        async fn save(&self, session: &SessionMeta) -> Result<()> {
            if self.should_fail.load(Ordering::SeqCst) {
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
        AppendMessageRequest {
            session_id: session_id.to_string(),
            role,
            content: content.to_string(),
            status,
            agent_profile_id: None,
            tool_state_snapshot: None,
            tool_state_deltas: Vec::new(),
            generation: None,
            title_if_first_message: title_if_first_message.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn append_message_saves_message_and_advances_session_tip() {
        with_test_store(
            "append-message-advances-tip",
            |message_store, session_store, _| async move {
                session_store
                    .save(&untitled_session("session", None, 1))
                    .await
                    .unwrap();
                let conversation_store =
                    ConversationStore::new(message_store.clone(), session_store.clone());

                let appended = conversation_store
                    .append_message(append_request(
                        "session",
                        ChatRole::User,
                        "hello",
                        MessageStatus::Complete,
                        Some("hello"),
                    ))
                    .await
                    .unwrap();

                assert_eq!(appended.previous_tip, None);
                let message = message_store
                    .get(&appended.message.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(message.parent_id, None);
                assert_eq!(message.role, ChatRole::User);
                assert_eq!(message.content, "hello");

                let stored_session = session_store.get("session").await.unwrap().unwrap();
                assert_eq!(stored_session.tip_id, Some(appended.message.id));
                assert_eq!(stored_session.title.as_deref(), Some("hello"));
                assert!(stored_session.updated_at >= 1);
            },
        )
        .await;
    }

    #[tokio::test]
    async fn append_message_keeps_existing_title_and_links_to_previous_tip() {
        with_test_store(
            "append-message-existing-title",
            |message_store, session_store, _| async move {
                session_store
                    .save(&untitled_session("session", None, 1))
                    .await
                    .unwrap();
                let conversation_store =
                    ConversationStore::new(message_store.clone(), session_store.clone());

                let first = conversation_store
                    .append_message(append_request(
                        "session",
                        ChatRole::User,
                        "first",
                        MessageStatus::Complete,
                        Some("first title"),
                    ))
                    .await
                    .unwrap();
                let second = conversation_store
                    .append_message(append_request(
                        "session",
                        ChatRole::User,
                        "second",
                        MessageStatus::Complete,
                        Some("second title"),
                    ))
                    .await
                    .unwrap();

                assert_eq!(second.previous_tip, Some(first.message.id.clone()));
                assert_eq!(second.message.parent_id, Some(first.message.id.clone()));
                let stored_session = session_store.get("session").await.unwrap().unwrap();
                assert_eq!(stored_session.tip_id, Some(second.message.id));
                assert_eq!(stored_session.title.as_deref(), Some("first title"));
            },
        )
        .await;
    }

    #[tokio::test]
    async fn append_message_session_save_failure_deletes_new_message() {
        let data_dir = test_dir("append-message-save-failure");
        tokio::fs::create_dir_all(&data_dir).await.unwrap();

        let message_store: Arc<dyn MessageStore> = Arc::new(FileMessageStore::new(&data_dir));
        let base_session_store: Arc<dyn SessionStore> =
            Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));
        base_session_store
            .save(&untitled_session("session", None, 1))
            .await
            .unwrap();

        let should_fail = Arc::new(AtomicBool::new(true));
        let failing_session_store: Arc<dyn SessionStore> = Arc::new(FailOnSaveSessionStore {
            inner: base_session_store.clone(),
            should_fail,
        });
        let conversation_store =
            ConversationStore::new(message_store.clone(), failing_session_store);

        let error = conversation_store
            .append_message(append_request(
                "session",
                ChatRole::User,
                "will roll back",
                MessageStatus::Complete,
                Some("will roll back"),
            ))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("intentional session save failure")
        );
        assert!(message_store.list_all_on_disk().await.unwrap().is_empty());
        assert_eq!(
            base_session_store
                .get("session")
                .await
                .unwrap()
                .unwrap()
                .tip_id,
            None
        );

        let _ = tokio::fs::remove_dir_all(&data_dir).await;
    }

    #[tokio::test]
    async fn concurrent_appends_do_not_silently_orphan_a_success() {
        with_test_store(
            "concurrent-appends",
            |message_store, session_store, _| async move {
                session_store
                    .save(&untitled_session("session", None, 1))
                    .await
                    .unwrap();
                let synchronized_messages: Arc<dyn MessageStore> =
                    Arc::new(BarrierOnSaveMessageStore {
                        inner: message_store.clone(),
                        barrier: Arc::new(Barrier::new(2)),
                    });
                let conversations =
                    ConversationStore::new(synchronized_messages, session_store.clone());

                let first = tokio::spawn({
                    let conversations = conversations.clone();
                    async move {
                        conversations
                            .append_message(append_request(
                                "session",
                                ChatRole::User,
                                "first",
                                MessageStatus::Complete,
                                None,
                            ))
                            .await
                    }
                });
                let second = tokio::spawn({
                    let conversations = conversations.clone();
                    async move {
                        conversations
                            .append_message(append_request(
                                "session",
                                ChatRole::User,
                                "second",
                                MessageStatus::Complete,
                                None,
                            ))
                            .await
                    }
                });

                let results = [first.await.unwrap(), second.await.unwrap()];
                assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
                assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);

                let stored_session = session_store.get("session").await.unwrap().unwrap();
                let tip = stored_session.tip_id.unwrap();
                assert!(message_store.exists(&tip).await.unwrap());
                assert_eq!(message_store.list_all_on_disk().await.unwrap().len(), 1);
            },
        )
        .await;
    }

    #[tokio::test]
    async fn append_and_rollback_race_cannot_restore_over_newer_tip() {
        with_test_store(
            "append-rollback-race",
            |_message_store, session_store, _| async move {
                let abandoned_id = MessageId::new("abandoned");
                let newer_id = MessageId::new("newer");
                let original = untitled_session("session", Some(&abandoned_id), 1);
                session_store.save(&original).await.unwrap();

                let mut append_update = original.clone();
                append_update.tip_id = Some(newer_id.clone());
                let mut stale_rollback = original;
                stale_rollback.tip_id = None;

                assert!(
                    session_store
                        .save_if_tip_matches(&append_update, Some(&abandoned_id))
                        .await
                        .unwrap()
                );
                assert!(
                    !session_store
                        .save_if_tip_matches(&stale_rollback, Some(&abandoned_id))
                        .await
                        .unwrap()
                );
                let stored_session = session_store.get("session").await.unwrap().unwrap();
                assert_eq!(stored_session.tip_id, Some(newer_id));
            },
        )
        .await;
    }

    #[tokio::test]
    async fn restore_tip_title_and_delete_message_requires_abandoned_message_to_be_tip() {
        with_test_store(
            "restore-tip-guard",
            |message_store, session_store, _| async move {
                session_store
                    .save(&untitled_session("session", None, 1))
                    .await
                    .unwrap();
                let conversation_store =
                    ConversationStore::new(message_store.clone(), session_store.clone());
                let root = conversation_store
                    .append_message(append_request(
                        "session",
                        ChatRole::User,
                        "root",
                        MessageStatus::Complete,
                        Some("root title"),
                    ))
                    .await
                    .unwrap();
                let streaming = conversation_store
                    .append_message(append_request(
                        "session",
                        ChatRole::Assistant,
                        "",
                        MessageStatus::Streaming {
                            call_id: kraai_types::CallId::new(Ulid::new()),
                        },
                        None,
                    ))
                    .await
                    .unwrap();

                let error = conversation_store
                    .restore_tip_title_and_delete_message(
                        "session",
                        &root.message.id,
                        root.previous_tip.clone(),
                        root.previous_title.clone(),
                    )
                    .await
                    .unwrap_err();
                assert!(error.to_string().contains("tip is not abandoned message"));
                assert!(message_store.exists(&root.message.id).await.unwrap());

                conversation_store
                    .restore_appended_message("session", &streaming)
                    .await
                    .unwrap();

                let stored_session = session_store.get("session").await.unwrap().unwrap();
                assert_eq!(stored_session.tip_id, Some(root.message.id.clone()));
                assert_eq!(stored_session.title.as_deref(), Some("root title"));
                assert!(!message_store.exists(&streaming.message.id).await.unwrap());
                assert!(message_store.exists(&root.message.id).await.unwrap());
            },
        )
        .await;
    }

    #[tokio::test]
    async fn restore_tip_title_succeeds_when_abandoned_message_cleanup_fails() {
        with_test_store(
            "restore-tip-delete-failure",
            |message_store, session_store, _| async move {
                session_store
                    .save(&untitled_session("session", None, 1))
                    .await
                    .unwrap();
                let should_fail = Arc::new(AtomicBool::new(true));
                let failing_message_store: Arc<dyn MessageStore> =
                    Arc::new(FailOnDeleteMessageStore {
                        inner: message_store.clone(),
                        should_fail,
                    });
                let conversation_store =
                    ConversationStore::new(failing_message_store, session_store.clone());
                let root = conversation_store
                    .append_message(append_request(
                        "session",
                        ChatRole::User,
                        "root",
                        MessageStatus::Complete,
                        Some("root title"),
                    ))
                    .await
                    .unwrap();
                let streaming = conversation_store
                    .append_message(append_request(
                        "session",
                        ChatRole::Assistant,
                        "",
                        MessageStatus::Streaming {
                            call_id: kraai_types::CallId::new(Ulid::new()),
                        },
                        None,
                    ))
                    .await
                    .unwrap();

                conversation_store
                    .restore_appended_message("session", &streaming)
                    .await
                    .unwrap();

                let stored_session = session_store.get("session").await.unwrap().unwrap();
                assert_eq!(stored_session.tip_id, Some(root.message.id));
                assert_eq!(stored_session.title.as_deref(), Some("root title"));
                assert!(message_store.exists(&streaming.message.id).await.unwrap());
            },
        )
        .await;
    }
}
