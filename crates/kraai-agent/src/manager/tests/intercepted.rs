use super::super::*;
use super::common::{cleanup_dir, test_manager};

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
                    Default::default(),
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
                Default::default(),
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
    let mut metadata = manager.session_store.get(&session).await?.expect("session");
    metadata.selected_profile_id = Some("missing-profile".into());
    manager.session_store.save(&metadata).await?;
    rusqlite::Connection::open(data_dir.join("kraai.sqlite3"))?.execute_batch(
        "CREATE TRIGGER fail_lease_release BEFORE UPDATE OF lease_active ON sessions
         WHEN NEW.lease_active = 0 BEGIN SELECT RAISE(FAIL, 'injected lease release failure'); END;",
    )?;
    let error = manager
        .prepare_messages_stream(
            &session,
            vec!["queued".into()],
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .expect_err("preparation fails");
    assert!(format!("{error:?}").contains("missing-profile"));
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
            Default::default(),
        )
        .await?;
    for messages in [Vec::new(), vec!["queued".into()]] {
        let rejected = manager
            .prepare_messages_stream(
                &session_id,
                messages,
                ModelId::new("new-model"),
                ProviderId::new("mock-alternate"),
                Default::default(),
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
async fn failed_preparation_appends_no_history() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let first = manager
        .prepare_start_stream(
            &session_id,
            "first".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await?;
    manager.complete_message(&first.message_id).await?;
    manager.clear_active_turn(&session_id);
    tokio::fs::write(data_dir.join(AGENTS_MD_FILE_NAME), [0xff]).await?;
    let messages = vec!["one".into(), "two".into(), "three".into()];
    let _error = manager
        .prepare_messages_stream(
            &session_id,
            messages.clone(),
            ModelId::new("new-model"),
            ProviderId::new("mock-alternate"),
            Default::default(),
        )
        .await
        .expect_err("preparation fails");
    assert!(!manager.is_turn_active(&session_id));
    let state = manager
        .session_states
        .get(&session_id)
        .expect("session state");
    assert_eq!(state.last_model, Some(ModelId::new("mock-model")));
    assert_eq!(state.last_provider, Some(ProviderId::new("mock")));
    assert_eq!(
        manager.get_tip(&session_id).await?,
        Some(first.message_id.clone())
    );

    tokio::fs::remove_file(data_dir.join(AGENTS_MD_FILE_NAME)).await?;
    let retry = manager
        .prepare_messages_stream(
            &session_id,
            messages,
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
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
    let history = manager.history_context(&retry.message_id).await?;
    let persisted_users: Vec<_> = history
        .iter()
        .rev()
        .filter_map(|message| match &message.content {
            ConversationItem::User { content } => Some(content.as_text().unwrap_or("")),
            _ => None,
        })
        .collect();
    assert_eq!(persisted_users, vec!["first", "one", "two", "three"]);
    assert_eq!(
        manager.get_tip(&session_id).await?,
        Some(retry.message_id.clone())
    );
    cleanup_dir(data_dir).await;
    Ok(())
}
