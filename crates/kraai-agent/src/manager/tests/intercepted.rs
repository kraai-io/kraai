use super::super::*;
use super::common::{cleanup_dir, open_persistence, test_manager};

#[tokio::test]
async fn empty_interception_releases_only_its_newly_claimed_turn() {
    for already_owned in [false, true] {
        let (manager, data_dir) = test_manager().await;
        let session = manager.create_session().await.unwrap();
        let observer = open_persistence(&data_dir).await;
        if already_owned {
            manager
                .persistence
                .sessions()
                .claim_turn(&session)
                .await
                .unwrap();
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
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            manager
                .persistence
                .sessions()
                .owns_turn(&session)
                .await
                .unwrap(),
            already_owned
        );
        if already_owned {
            assert!(observer.sessions().claim_turn(&session).await.is_err());
            manager
                .persistence
                .sessions()
                .release_turn(&session)
                .await
                .unwrap();
        } else {
            observer.sessions().claim_turn(&session).await.unwrap();
            observer.sessions().release_turn(&session).await.unwrap();
        }
        cleanup_dir(data_dir).await;
    }
}

#[tokio::test]
async fn failed_profile_resolution_releases_a_newly_claimed_turn() {
    let (manager, data_dir) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let mut metadata = manager
        .session_store
        .get(&session)
        .await
        .unwrap()
        .expect("session");
    metadata.selected_profile_id = Some("missing-profile".into());
    manager.session_store.save(&metadata).await.unwrap();
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
    assert!(
        !manager
            .persistence
            .sessions()
            .owns_turn(&session)
            .await
            .unwrap()
    );
    let observer = open_persistence(&data_dir).await;
    observer.sessions().claim_turn(&session).await.unwrap();
    assert!(manager.get_chat_history(&session).await.unwrap().is_empty());
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn preparation_error_survives_lease_release_failure() {
    let (manager, data_dir) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let mut metadata = manager
        .session_store
        .get(&session)
        .await
        .unwrap()
        .expect("session");
    metadata.selected_profile_id = Some("missing-profile".into());
    manager.session_store.save(&metadata).await.unwrap();
    rusqlite::Connection::open(data_dir.join("kraai.sqlite3")).unwrap().execute_batch(
        "CREATE TRIGGER fail_lease_release BEFORE UPDATE OF lease_active ON sessions
         WHEN NEW.lease_active = 0 BEGIN SELECT RAISE(FAIL, 'injected lease release failure'); END;",
    ).unwrap();
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
    assert!(manager.get_chat_history(&session).await.unwrap().is_empty());
    assert!(
        manager
            .persistence
            .sessions()
            .observe(&session)
            .await
            .unwrap()
            .expect("lease")
            .lease_active
    );
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn rejected_interception_preserves_active_model_and_provider() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let first = manager
        .prepare_start_stream(
            &session_id,
            "first".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    for messages in [Vec::new(), vec!["queued".into()]] {
        let rejected = manager
            .prepare_messages_stream(
                &session_id,
                messages,
                ModelId::new("new-model"),
                ProviderId::new("mock-alternate"),
                Default::default(),
            )
            .await
            .unwrap();
        assert!(rejected.is_none());
        assert!(
            manager
                .persistence
                .sessions()
                .owns_turn(&session_id)
                .await
                .unwrap()
        );
    }
    manager.complete_message(&first.message_id).await.unwrap();
    let continuation = manager
        .prepare_continuation_stream(&session_id)
        .await
        .unwrap()
        .expect("continuation");
    assert_eq!(continuation.model_id, ModelId::new("mock-model"));
    assert_eq!(continuation.provider_id, ProviderId::new("mock"));
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn failed_preparation_appends_no_history() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let first = manager
        .prepare_start_stream(
            &session_id,
            "first".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager.complete_message(&first.message_id).await.unwrap();
    manager.clear_active_turn(&session_id);
    tokio::fs::write(data_dir.join(AGENTS_MD_FILE_NAME), [0xff])
        .await
        .unwrap();
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
    let (last_model, last_provider) = lock(&manager.session_states)
        .get(&session_id)
        .map(|state| (state.last_model.clone(), state.last_provider.clone()))
        .expect("session state");
    assert_eq!(last_model, Some(ModelId::new("mock-model")));
    assert_eq!(last_provider, Some(ProviderId::new("mock")));
    assert_eq!(
        manager.get_tip(&session_id).await.unwrap(),
        Some(first.message_id.clone())
    );

    tokio::fs::remove_file(data_dir.join(AGENTS_MD_FILE_NAME))
        .await
        .unwrap();
    let retry = manager
        .prepare_messages_stream(
            &session_id,
            messages,
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap()
        .expect("retry");
    let users: Vec<_> = retry
        .provider_request
        .messages
        .iter()
        .filter_map(|item| {
            if let ConversationItem::User { content } = item {
                Some(content.as_text().unwrap_or(""))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(users, vec!["first", "one", "two", "three"]);
    let history = manager.history_context(&retry.message_id).await.unwrap();
    let persisted_users: Vec<_> = history
        .iter()
        .rev()
        .filter_map(|message| {
            if let ConversationItem::User { content } = &message.content {
                Some(content.as_text().unwrap_or(""))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(persisted_users, vec!["first", "one", "two", "three"]);
    assert_eq!(
        manager.get_tip(&session_id).await.unwrap(),
        Some(retry.message_id.clone())
    );
    cleanup_dir(data_dir).await;
}
