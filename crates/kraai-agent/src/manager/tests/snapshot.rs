use super::super::*;
use super::common::{cleanup_dir, test_manager};

#[tokio::test]
async fn history_readers_preserve_duplicate_persisted_ids_and_captured_stream_precedence()
-> Result<()> {
    for with_stream in [false, true] {
        let (mut manager, data_dir) = test_manager().await;
        let session_id = manager.create_session().await?;
        let oldest = manager
            .add_message(&session_id, ChatRole::User, "oldest".into(), None)
            .await?;
        let (duplicate_id, expected) = if with_stream {
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
                    "captured",
                )
                .await;
            (
                request.message_id,
                ConversationItem::Assistant {
                    items: vec![AssistantItem::Text {
                        phase: AssistantPhase::FinalAnswer,
                        text: "captured".into(),
                    }],
                },
            )
        } else {
            (
                oldest,
                ConversationItem::User {
                    content: "newest".into(),
                },
            )
        };
        let newest = manager
            .add_message(&session_id, ChatRole::User, "newest".into(), None)
            .await?;
        let mut message = manager.message_store.get(&newest).await?.unwrap();
        message.id = duplicate_id.clone();
        manager.message_store.unload(&newest).await;
        tokio::fs::write(
            data_dir.join("messages").join(format!("{newest}.json")),
            serde_json::to_vec(&message)?,
        )
        .await?;

        let snapshot = manager
            .capture_session_snapshot(&session_id)
            .await?
            .load()
            .await?;

        assert_eq!(
            snapshot.history.get(&duplicate_id).unwrap().content,
            expected
        );
        assert_eq!(
            manager
                .get_chat_history(&session_id)
                .await?
                .get(&duplicate_id)
                .unwrap()
                .content,
            expected
        );
        cleanup_dir(data_dir).await;
    }
    Ok(())
}

#[tokio::test]
async fn snapshot_rejects_a_parent_cycle() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("cycle").into(),
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
    assert!(snapshot.streaming);
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
async fn snapshot_keeps_captured_streams_outside_the_tip_chain() -> Result<()> {
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
    let parent_id = manager
        .message_store
        .get(&request.message_id)
        .await?
        .unwrap()
        .parent_id;
    manager.set_tip(&session_id, parent_id.clone()).await?;
    let reader = manager.capture_session_snapshot(&session_id).await?;

    let snapshot = reader.load().await?;

    assert_eq!(snapshot.session.tip_id, parent_id);
    assert!(snapshot.streaming);
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
