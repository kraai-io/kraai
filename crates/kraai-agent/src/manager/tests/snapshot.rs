use super::super::*;
use super::common::{cleanup_dir, test_manager};

#[tokio::test]
async fn snapshot_rejects_a_parent_cycle() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("cycle"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager.complete_message(&request.message_id).await?;
    let message = manager
        .message_store
        .get(&request.message_id)
        .await?
        .unwrap();
    let parent_id = message.parent_id.unwrap();
    let mut parent = manager.message_store.get(&parent_id).await?.unwrap();
    parent.parent_id = Some(request.message_id.clone());
    manager.message_store.save(&parent).await?;
    let reader = manager.capture_session_snapshot(&session_id).await?;

    let error = reader.load().await.err().unwrap();

    assert!(
        error
            .to_string()
            .contains(&format!("cycle repeats message {}", request.message_id))
    );
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn captured_snapshot_keeps_stream_contents_and_tip_across_completion() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let request = manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager
        .append_text_chunk(
            &request.message_id,
            "item",
            AssistantPhase::FinalAnswer,
            "before",
        )
        .await;
    let reader = manager.capture_session_snapshot(&session_id).await?;
    assert!(reader.streaming);
    manager
        .append_text_chunk(
            &request.message_id,
            "item",
            AssistantPhase::FinalAnswer,
            " after",
        )
        .await;
    manager.complete_message(&request.message_id).await?;
    manager.clear_active_turn(&session_id);
    manager
        .set_session_profile(&session_id, "plan".into())
        .await?;

    let snapshot = reader.load().await?;
    let captured = snapshot
        .history
        .get(&request.message_id)
        .expect("captured stream");
    assert_eq!(
        captured.content.assistant_items(),
        Some(
            [AssistantItem::Text {
                phase: AssistantPhase::FinalAnswer,
                text: "before".into(),
            }]
            .as_slice()
        )
    );
    assert_ne!(captured.status, MessageStatus::Complete);
    assert_eq!(
        snapshot.profiles.selected_profile_id.as_deref(),
        Some("coding")
    );
    assert!(snapshot.profiles.profile_locked);
    assert!(snapshot.context_usage.is_none());
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn captured_snapshot_survives_deletion_of_an_aborted_stream() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let request = manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    let reader = manager.capture_session_snapshot(&session_id).await?;
    manager.abort_streaming_message(&request.message_id).await?;

    let snapshot = reader.load().await?;
    assert!(snapshot.history.contains_key(&request.message_id));
    assert_eq!(snapshot.history.len(), 2);
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn snapshot_reports_missing_history_instead_of_returning_partial_state() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let request = manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager.complete_message(&request.message_id).await?;
    manager.clear_active_turn(&session_id);
    let reader = manager.capture_session_snapshot(&session_id).await?;
    manager.delete_session(&session_id).await?;
    assert!(reader.load().await.is_err());
    cleanup_dir(data_dir).await;
    Ok(())
}
