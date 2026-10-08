use super::super::*;
use super::common::{cleanup_dir, test_manager};
use std::sync::atomic::{AtomicUsize, Ordering};

struct LimitedWrites {
    inner: Arc<dyn SessionStore>,
    messages: Arc<dyn MessageStore>,
    remaining: AtomicUsize,
}

#[tokio::test]
async fn empty_interception_releases_only_its_newly_claimed_turn() -> Result<()> {
    for already_owned in [false, true] {
        let (mut manager, data_dir) = test_manager().await;
        let session = manager.create_session().await?;
        let observer = kraai_persistence::Persistence::open(&data_dir).await?;
        if already_owned {
            manager.persistence.sessions().claim_turn(&session).await?;
        }
        assert!(
            manager
                .prepare_messages_stream(
                    &session,
                    Vec::new(),
                    ModelId::new("mock-model"),
                    ProviderId::new("mock"),
                )
                .await?
                .is_none()
        );
        assert_eq!(
            manager.persistence.sessions().owns_turn(&session).await?,
            already_owned
        );
        if already_owned {
            assert!(observer.sessions().claim_turn(&session).await.is_err());
            manager
                .persistence
                .sessions()
                .release_turn(&session)
                .await?;
        } else {
            observer.sessions().claim_turn(&session).await?;
            observer.sessions().release_turn(&session).await?;
        }
        cleanup_dir(data_dir).await;
    }
    Ok(())
}

#[tokio::test]
async fn failed_profile_resolution_releases_a_newly_claimed_turn() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session = manager.create_session().await?;
    let mut metadata = manager.session_store.get(&session).await?.expect("session");
    metadata.selected_profile_id = Some("missing-profile".into());
    manager.session_store.save(&metadata).await?;
    assert!(
        manager
            .prepare_messages_stream(
                &session,
                vec!["queued".into()],
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await
            .is_err()
    );
    assert!(!manager.persistence.sessions().owns_turn(&session).await?);
    let observer = kraai_persistence::Persistence::open(&data_dir).await?;
    observer.sessions().claim_turn(&session).await?;
    assert!(manager.get_chat_history(&session).await?.is_empty());
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn preparation_error_survives_lease_release_failure() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session = manager.create_session().await?;
    rusqlite::Connection::open(data_dir.join("kraai.sqlite3"))?.execute_batch(
        "CREATE TRIGGER fail_lease_release BEFORE UPDATE OF lease_active ON sessions
         WHEN NEW.lease_active = 0 BEGIN SELECT RAISE(FAIL, 'injected lease release failure'); END;",
    )?;
    let error = manager
        .prepare_messages_stream(
            &session,
            vec!["queued".into()],
            ModelId::new("mock-model"),
            ProviderId::new("missing-provider"),
        )
        .await
        .expect_err("preparation fails");
    assert!(format!("{error:?}").contains("missing-provider"));
    assert!(!format!("{error:?}").contains("injected lease release failure"));
    assert!(manager.get_chat_history(&session).await?.is_empty());
    assert!(
        manager
            .persistence
            .sessions()
            .observe(&session)
            .await?
            .expect("lease")
            .lease_active
    );
    cleanup_dir(data_dir).await;
    Ok(())
}

impl LimitedWrites {
    fn consume_write(&self) -> Result<()> {
        self.remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .map(|_| ())
            .map_err(|_remaining| eyre!("injected session write failure"))
    }
}

#[async_trait::async_trait]
impl SessionStore for LimitedWrites {
    async fn link_message_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
    ) -> Result<bool> {
        self.consume_write()?;
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
        self.inner.save(session).await
    }
    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await
    }
    async fn save_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
    ) -> Result<bool> {
        self.consume_write()?;
        self.inner.save_if_tip_matches(session, expected_tip).await
    }
}

#[async_trait::async_trait]
impl MessageStore for LimitedWrites {
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        self.messages.get(id).await
    }
    async fn save(&self, message: &Message) -> Result<()> {
        self.messages.save(message).await
    }
    async fn delete(&self, id: &MessageId) -> Result<()> {
        self.messages.delete(id).await
    }
    async fn exists(&self, id: &MessageId) -> Result<bool> {
        self.messages.exists(id).await
    }
    async fn list_ids(&self) -> Result<HashSet<MessageId>> {
        self.messages.list_ids().await
    }
}

#[tokio::test]
async fn rejected_interception_preserves_active_model_and_provider() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let first = manager
        .prepare_start_stream(
            &session_id,
            "first".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    for messages in [Vec::new(), vec!["queued".into()]] {
        let rejected = manager
            .prepare_messages_stream(
                &session_id,
                messages,
                ModelId::new("new-model"),
                ProviderId::new("mock-alternate"),
            )
            .await?;
        assert!(rejected.is_none());
        assert!(
            manager
                .persistence
                .sessions()
                .owns_turn(&session_id)
                .await?
        );
    }
    manager.complete_message(&first.message_id).await?;
    let continuation = manager
        .prepare_continuation_stream(&session_id)
        .await?
        .expect("continuation");
    assert_eq!(continuation.model_id, ModelId::new("mock-model"));
    assert_eq!(continuation.provider_id, ProviderId::new("mock"));
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn partial_rollback_blocks_preparation_until_history_is_restored() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let first = manager
        .prepare_start_stream(
            &session_id,
            "first".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager.complete_message(&first.message_id).await?;
    manager.clear_active_turn(&session_id);
    let store = Arc::new(LimitedWrites {
        inner: manager.session_store.clone(),
        messages: manager.message_store.clone(),
        // Append three messages, restore the last, then fail the remaining rollback.
        remaining: AtomicUsize::new(4),
    });
    manager.conversation_store = ConversationStore::new(store.clone(), store.clone());
    let messages = vec!["one".into(), "two".into(), "three".into()];
    let error = manager
        .prepare_messages_stream(
            &session_id,
            messages.clone(),
            ModelId::new("new-model"),
            ProviderId::new("missing"),
        )
        .await
        .expect_err("preparation and rollback fail");
    assert!(format!("{error:?}").contains("rollback is incomplete"));
    assert!(!manager.is_turn_active(&session_id));
    let state = manager
        .session_states
        .get(&session_id)
        .expect("session state");
    assert_eq!(state.last_model, Some(ModelId::new("mock-model")));
    assert_eq!(state.last_provider, Some(ProviderId::new("mock")));
    let partial_tip = manager.get_tip(&session_id).await?;
    assert_ne!(partial_tip, Some(first.message_id.clone()));
    assert!(manager.undo_last_user_message(&session_id).await.is_err());
    assert!(
        manager
            .prepare_continuation_stream(&session_id)
            .await
            .is_err()
    );
    assert!(
        manager
            .prepare_start_stream(
                &session_id,
                "new".into(),
                ModelId::new("mock-model"),
                ProviderId::new("mock")
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .prepare_messages_stream(
                &session_id,
                messages.clone(),
                ModelId::new("mock-model"),
                ProviderId::new("mock")
            )
            .await
            .is_err()
    );
    assert_eq!(manager.get_tip(&session_id).await?, partial_tip);

    store.remaining.store(usize::MAX, Ordering::SeqCst);
    let retry = manager
        .prepare_messages_stream(
            &session_id,
            messages,
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?
        .expect("retry");
    let users: Vec<_> = retry
        .provider_request
        .messages
        .iter()
        .filter_map(|item| match item {
            ConversationItem::User { content: text } => Some(text.as_text().unwrap_or("")),
            _ => None,
        })
        .collect();
    assert_eq!(users, vec!["first", "one", "two", "three"]);
    assert!(!manager.pending_message_rollbacks.contains_key(&session_id));
    cleanup_dir(data_dir).await;
    Ok(())
}
