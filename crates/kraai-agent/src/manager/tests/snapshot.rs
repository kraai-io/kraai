use super::super::*;
use super::common::{cleanup_dir, test_manager};

#[tokio::test]
async fn history_readers_preserve_duplicate_persisted_ids_and_captured_stream_precedence() {
    for with_stream in [false, true] {
        let (manager, data_dir) = test_manager().await;
        let session_id = manager.create_session().await.unwrap();
        let oldest = manager
            .add_message(&session_id, ChatRole::User, "oldest".into(), None)
            .await
            .unwrap();
        let (duplicate_id, expected) = if with_stream {
            let request = manager
                .prepare_start_stream(
                    &session_id,
                    "hello".into(),
                    ModelId::new("mock-model"),
                    ProviderId::new("mock"),
                    Default::default(),
                )
                .await
                .unwrap();
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
            .await
            .unwrap();
        let mut message = manager.message_store.get(&newest).await.unwrap().unwrap();
        message.id = duplicate_id.clone();

        super::common::corrupt_message(
            &data_dir,
            &newest,
            &serde_json::to_string(&message).unwrap(),
        );

        let snapshot = manager
            .capture_session_snapshot(&session_id)
            .await
            .unwrap()
            .load()
            .await
            .unwrap();

        assert_eq!(
            snapshot.history.get(&duplicate_id).unwrap().content,
            expected
        );
        assert_eq!(
            manager
                .get_chat_history(&session_id)
                .await
                .unwrap()
                .get(&duplicate_id)
                .unwrap()
                .content,
            expected
        );
        cleanup_dir(data_dir).await;
    }
}

#[tokio::test]
async fn snapshot_rejects_a_parent_cycle() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("cycle").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager.complete_message(&request.message_id).await.unwrap();
    let message = manager
        .message_store
        .get(&request.message_id)
        .await
        .unwrap()
        .unwrap();
    let parent_id = message.parent_id.unwrap();
    let mut parent = manager
        .message_store
        .get(&parent_id)
        .await
        .unwrap()
        .unwrap();
    parent.parent_id = Some(request.message_id.clone());
    manager.message_store.save(&parent).await.unwrap();
    let error = manager
        .capture_session_snapshot(&session_id)
        .await
        .err()
        .unwrap();

    assert!(
        error
            .to_string()
            .contains(&format!("cycle repeats message {}", request.message_id))
    );
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn captured_snapshot_keeps_stream_contents_and_tip_across_completion() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let request = manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .append_text_chunk(
            &request.message_id,
            "item",
            AssistantPhase::FinalAnswer,
            "before",
        )
        .await;
    let reader = manager.capture_session_snapshot(&session_id).await.unwrap();
    manager
        .append_text_chunk(
            &request.message_id,
            "item",
            AssistantPhase::FinalAnswer,
            " after",
        )
        .await;
    manager.complete_message(&request.message_id).await.unwrap();
    manager.clear_active_turn(&session_id);
    manager
        .set_session_profile(&session_id, "coding-no-sandbox".into())
        .await
        .unwrap();

    let snapshot = reader.load().await.unwrap();
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
}

#[tokio::test]
async fn captured_snapshot_survives_deletion_of_an_aborted_stream() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let request = manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    let reader = manager.capture_session_snapshot(&session_id).await.unwrap();
    manager
        .abort_streaming_message(&request.message_id)
        .await
        .unwrap();

    let snapshot = reader.load().await.unwrap();
    assert!(snapshot.history.contains_key(&request.message_id));
    assert_eq!(snapshot.history.len(), 2);
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn snapshot_keeps_captured_streams_outside_the_tip_chain() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let request = manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    let parent_id = manager
        .message_store
        .get(&request.message_id)
        .await
        .unwrap()
        .unwrap()
        .parent_id;
    manager
        .set_tip(&session_id, parent_id.clone())
        .await
        .unwrap();
    let reader = manager.capture_session_snapshot(&session_id).await.unwrap();

    let snapshot = reader.load().await.unwrap();

    assert_eq!(snapshot.session.tip_id, parent_id);
    assert!(snapshot.streaming);
    assert!(snapshot.history.contains_key(&request.message_id));
    assert_eq!(snapshot.history.len(), 2);
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn snapshot_reports_missing_history_instead_of_returning_partial_state() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let request = manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager.complete_message(&request.message_id).await.unwrap();
    manager.clear_active_turn(&session_id);
    manager
        .message_store
        .delete(&request.message_id)
        .await
        .unwrap();
    assert!(manager.capture_session_snapshot(&session_id).await.is_err());
    cleanup_dir(data_dir).await;
}
