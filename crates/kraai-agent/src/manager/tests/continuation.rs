use super::super::*;
use super::common::{cleanup_dir, test_manager};

#[tokio::test]
async fn explicit_continuation_matches_message_submission_after_restoring_session() {
    let (manager, data_dir) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let first = manager
        .prepare_start_stream(
            &session,
            "first".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .append_text_chunk(
            &first.message_id,
            "answer",
            AssistantPhase::FinalAnswer,
            "answer",
        )
        .await;
    manager.complete_message(&first.message_id).await.unwrap();
    manager.clear_active_turn(&session);
    lock(&manager.session_states).remove(&session);

    let continuation = manager
        .prepare_messages_stream(
            &session,
            Vec::new(),
            ModelId::new("new-model"),
            ProviderId::new("mock-alternate"),
            Default::default(),
        )
        .await
        .unwrap()
        .expect("restored session can continue");
    assert_eq!(continuation.model_id.as_str(), "new-model");
    assert_eq!(continuation.provider_id.as_str(), "mock-alternate");
    let history = manager.get_chat_history(&session).await.unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(
        history
            .get(&continuation.message_id)
            .expect("placeholder")
            .parent_id
            .as_ref(),
        Some(&first.message_id)
    );
    manager
        .abort_streaming_message(&continuation.message_id)
        .await
        .unwrap();
    manager.clear_active_turn(&session);

    let normal = manager
        .prepare_start_stream(
            &session,
            "next".into(),
            ModelId::new("new-model"),
            ProviderId::new("mock-alternate"),
            Default::default(),
        )
        .await
        .unwrap();
    let mut normal_history = normal.provider_request.messages;
    assert_eq!(
        normal_history.pop(),
        Some(ConversationItem::User {
            content: "next".into()
        })
    );
    assert_eq!(continuation.provider_request.messages, normal_history);
    assert_eq!(
        continuation.provider_request.script_tool,
        normal.provider_request.script_tool
    );
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn failed_explicit_continuation_restores_model_and_history() {
    let (manager, data_dir) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let first = manager
        .prepare_start_stream(
            &session,
            "first".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager.complete_message(&first.message_id).await.unwrap();
    manager.clear_active_turn(&session);
    assert!(
        manager
            .prepare_messages_stream(
                &session,
                Vec::new(),
                ModelId::new("new-model"),
                ProviderId::new("missing"),
                Default::default(),
            )
            .await
            .is_err()
    );
    assert!(!manager.is_turn_active(&session));
    assert_eq!(
        manager.get_tip(&session).await.unwrap(),
        Some(first.message_id)
    );
    let state = lock(&manager.session_states)
        .get(&session)
        .cloned()
        .expect("session state");
    assert_eq!(
        state.last_model.as_ref().map(ModelId::as_str),
        Some("mock-model")
    );
    assert_eq!(
        state.last_provider.as_ref().map(ProviderId::as_str),
        Some("mock")
    );
    cleanup_dir(data_dir).await;
}
