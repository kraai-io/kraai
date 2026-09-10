use super::super::*;
use super::common::{cleanup_dir, test_manager};
use std::sync::atomic::{AtomicUsize, Ordering};

struct LimitedWrites {
    inner: Arc<dyn SessionStore>,
    remaining: AtomicUsize,
}

#[async_trait::async_trait]
impl SessionStore for LimitedWrites {
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
        if self
            .remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_err()
        {
            return Err(eyre!("injected session write failure"));
        }
        self.inner.save_if_tip_matches(session, expected_tip).await
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
    let rejected = manager
        .prepare_intercepted_stream(
            &session_id,
            vec!["queued".into()],
            ModelId::new("new-model"),
            ProviderId::new("mock-alternate"),
        )
        .await?;
    assert!(rejected.is_none());
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
        // Append three messages, restore the last, then fail the remaining rollback.
        remaining: AtomicUsize::new(4),
    });
    manager.conversation_store =
        ConversationStore::new(manager.message_store.clone(), store.clone());
    let messages = vec!["one".to_string(), "two".to_string(), "three".to_string()];
    let error = manager
        .prepare_intercepted_stream(
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
            .prepare_intercepted_stream(
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
        .prepare_intercepted_stream(
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
            ConversationItem::User { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(users, vec!["first", "one", "two", "three"]);
    assert!(!manager.pending_message_rollbacks.contains_key(&session_id));
    cleanup_dir(data_dir).await;
    Ok(())
}
