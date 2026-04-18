use super::super::*;
use super::common::{cleanup_dir, test_manager};
use color_eyre::eyre::Result;

#[tokio::test]
async fn duplicate_continuation_trigger_is_ignored_while_stream_is_active() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    manager
        .set_session_profile(&session_id, String::from("plan-code"))
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
        .set_session_profile(&session_id, String::from("plan-code"))
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
