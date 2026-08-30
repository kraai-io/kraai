use super::super::*;
use super::common::{cleanup_dir, test_manager};
use color_eyre::eyre::Result;

#[tokio::test]
async fn duplicate_continuation_trigger_is_ignored_while_stream_is_active() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    manager
        .set_session_profile(&session_id, String::from("plan"))
        .await?;

    let first_request = manager
        .prepare_start_stream(
            &session_id,
            String::from("first"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager.complete_message(&first_request.message_id).await?;

    let continuation = manager
        .prepare_continuation_stream(&session_id)
        .await?
        .expect("continuation request should exist");

    let duplicate = manager.prepare_continuation_stream(&session_id).await?;
    assert!(duplicate.is_none());
    assert!(manager.is_turn_active(&session_id));

    manager.complete_message(&continuation.message_id).await?;

    let next_continuation = manager
        .prepare_continuation_stream(&session_id)
        .await?
        .expect("session should continue working after duplicate trigger");
    assert_ne!(next_continuation.message_id, continuation.message_id);

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn prepare_continuation_restarts_a_new_turn_after_previous_turn_is_cleared() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    manager
        .set_session_profile(&session_id, String::from("plan"))
        .await?;

    let first_request = manager
        .prepare_start_stream(
            &session_id,
            String::from("first"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager.complete_message(&first_request.message_id).await?;
    manager.clear_active_turn(&session_id);

    let continuation = manager
        .prepare_continuation_stream(&session_id)
        .await?
        .expect("continuation request should exist after clearing the previous turn");

    assert!(manager.is_turn_active(&session_id));
    assert_ne!(continuation.message_id, first_request.message_id);

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn cancelled_output_remains_semantic_when_the_next_turn_switches_provider() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;

    let first = manager
        .prepare_start_stream(
            &session_id,
            String::from("start with the first provider"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    assert_eq!(
        manager
            .append_text_chunk(
                &first.message_id,
                "commentary-1",
                AssistantPhase::Commentary,
                "I am partway through.",
            )
            .await
            .as_deref(),
        Some("I am partway through.")
    );
    manager.cancel_streaming_message(&first.message_id).await?;
    manager.clear_active_turn(&session_id);

    let second = manager
        .prepare_start_stream(
            &session_id,
            String::from("continue with a different provider"),
            ModelId::new("mock-model"),
            ProviderId::new("mock-alternate"),
        )
        .await?;

    assert_eq!(second.provider_id, ProviderId::new("mock-alternate"));
    assert!(second.provider_request.messages.iter().any(|item| {
        matches!(
            item,
            ConversationItem::Assistant { items }
                if items == &vec![AssistantItem::Text {
                    phase: AssistantPhase::Commentary,
                    text: String::from("I am partway through."),
                }]
        )
    }));

    cleanup_dir(data_dir).await;
    Ok(())
}
