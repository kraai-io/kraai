use super::super::*;
use super::common::{cleanup_dir, test_manager};

#[tokio::test]
async fn context_usage_selects_the_latest_complete_assistant_and_validates_older_messages() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let oldest = manager
        .add_message(&session_id, ChatRole::User, "oldest".into(), None)
        .await
        .unwrap();
    for (role, status, total_tokens) in [
        (ChatRole::Assistant, MessageStatus::Complete, Some(10)),
        (ChatRole::Assistant, MessageStatus::Complete, Some(20)),
        (ChatRole::Assistant, MessageStatus::Complete, None),
        (ChatRole::User, MessageStatus::Complete, Some(40)),
        (
            ChatRole::Assistant,
            MessageStatus::Streaming {
                stream_id: StreamId::new(Ulid::generate()),
            },
            Some(30),
        ),
    ] {
        let id = manager
            .add_message(&session_id, role, "message".into(), None)
            .await
            .unwrap();
        let mut message = manager.message_store.get(&id).await.unwrap().unwrap();
        message.status = status;
        message.generation = Some(MessageGeneration {
            options: Default::default(),
            provider_id: ProviderId::new("mock"),
            model_id: ModelId::new("mock-model"),
            max_context: Some(100),
            usage: total_tokens.map(|total_tokens| TokenUsage {
                total_tokens,
                ..Default::default()
            }),
        });
        manager.message_store.save(&message).await.unwrap();
    }

    let usage = manager
        .get_session_context_usage(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(usage.usage.total_tokens, 20);
    assert_eq!(usage.max_context, Some(100));
    assert_eq!(
        manager
            .capture_session_snapshot(&session_id)
            .await
            .unwrap()
            .load()
            .await
            .unwrap()
            .context_usage,
        Some(usage)
    );

    let mut oldest_message = manager.message_store.get(&oldest).await.unwrap().unwrap();
    manager.message_store.delete(&oldest).await.unwrap();
    assert!(
        manager
            .get_session_context_usage(&session_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("disappeared while reading session history")
    );
    super::common::corrupt_message(&data_dir, &oldest, "{}");
    assert!(
        manager
            .get_session_context_usage(&session_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("missing field")
    );
    oldest_message.parent_id = manager.get_tip(&session_id).await.unwrap();
    manager.message_store.save(&oldest_message).await.unwrap();
    assert!(
        manager
            .get_session_context_usage(&session_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("cycle repeats message")
    );
    cleanup_dir(data_dir).await;
}
