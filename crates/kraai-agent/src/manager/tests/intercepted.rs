use super::super::*;
use super::common::{cleanup_dir, test_manager};

#[tokio::test]
async fn rejected_interception_preserves_active_model_and_provider() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    manager
        .persistence
        .sessions()
        .claim_turn(&session_id)
        .await
        .unwrap();
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
