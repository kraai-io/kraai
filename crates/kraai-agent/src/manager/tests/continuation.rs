use super::super::*;
use super::common::{cleanup_dir, test_manager};

#[tokio::test]
async fn explicit_continuation_matches_message_submission_after_restoring_session() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session = manager.create_session().await?;
    let first = manager
        .prepare_start_stream(
            &session,
            "first".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager
        .append_text_chunk(
            &first.message_id,
            "answer",
            AssistantPhase::FinalAnswer,
            "answer",
        )
        .await;
    manager.complete_message(&first.message_id).await?;
    manager.clear_active_turn(&session);
    manager.session_states.remove(&session);

    let continuation = manager
        .prepare_messages_stream(
            &session,
            Vec::new(),
            ModelId::new("new-model"),
            ProviderId::new("mock-alternate"),
        )
        .await?
        .expect("restored session can continue");
    assert_eq!(continuation.model_id.as_str(), "new-model");
    assert_eq!(continuation.provider_id.as_str(), "mock-alternate");
    let history = manager.get_chat_history(&session).await?;
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
        .await?;
    manager.clear_active_turn(&session);

    let normal = manager
        .prepare_start_stream(
            &session,
            "next".into(),
            ModelId::new("new-model"),
            ProviderId::new("mock-alternate"),
        )
        .await?;
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
    assert_eq!(
        continuation.provider_request.cacheable_messages,
        normal.provider_request.cacheable_messages
    );
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn failed_explicit_continuation_restores_model_and_history() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session = manager.create_session().await?;
    let first = manager
        .prepare_start_stream(
            &session,
            "first".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager.complete_message(&first.message_id).await?;
    manager.clear_active_turn(&session);
    assert!(
        manager
            .prepare_messages_stream(
                &session,
                Vec::new(),
                ModelId::new("new-model"),
                ProviderId::new("missing")
            )
            .await
            .is_err()
    );
    assert!(!manager.is_turn_active(&session));
    assert_eq!(manager.get_tip(&session).await?, Some(first.message_id));
    let state = manager.session_states.get(&session).expect("session state");
    assert_eq!(
        state.last_model.as_ref().map(ModelId::as_str),
        Some("mock-model")
    );
    assert_eq!(
        state.last_provider.as_ref().map(ProviderId::as_str),
        Some("mock")
    );
    cleanup_dir(data_dir).await;
    Ok(())
}
