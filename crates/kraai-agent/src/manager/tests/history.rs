use super::super::*;
use super::common::{cleanup_dir, test_manager};

#[tokio::test]
async fn context_usage_selects_the_latest_complete_assistant_and_validates_older_messages()
-> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let oldest = manager
        .add_message(&session_id, ChatRole::User, "oldest".into(), None)
        .await?;
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
            .await?;
        let mut message = manager.message_store.get(&id).await?.unwrap();
        message.status = status;
        message.generation = Some(MessageGeneration {
            provider_id: ProviderId::new("mock"),
            model_id: ModelId::new("mock-model"),
            max_context: Some(100),
            usage: total_tokens.map(|total_tokens| TokenUsage {
                total_tokens,
                ..Default::default()
            }),
        });
        manager.message_store.save(&message).await?;
    }

    let usage = manager
        .get_session_context_usage(&session_id)
        .await?
        .unwrap();
    assert_eq!(usage.usage.total_tokens, 20);
    assert_eq!(usage.max_context, Some(100));
    assert_eq!(
        manager
            .capture_session_snapshot(&session_id)
            .await?
            .load()
            .await?
            .context_usage,
        Some(usage)
    );

    let mut oldest_message = manager.message_store.get(&oldest).await?.unwrap();
    manager.message_store.delete(&oldest).await?;
    assert!(
        manager
            .get_session_context_usage(&session_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("disappeared while reading session history")
    );
    tokio::fs::write(
        data_dir.join("messages").join(format!("{oldest}.json")),
        "{}",
    )
    .await?;
    assert!(
        manager
            .get_session_context_usage(&session_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("Failed to parse message file")
    );
    oldest_message.parent_id = manager.get_tip(&session_id).await?;
    manager.message_store.save(&oldest_message).await?;
    assert!(
        manager
            .get_session_context_usage(&session_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("cycle repeats message")
    );
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn limited_user_history_still_reads_and_caches_the_entire_session() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let oldest = manager
        .add_message(&session_id, ChatRole::User, "oldest".into(), None)
        .await?;
    let assistant = manager
        .add_message(&session_id, ChatRole::Assistant, "reply".into(), None)
        .await?;
    let newest = manager
        .add_message(&session_id, ChatRole::User, "newest".into(), None)
        .await?;
    let ids = [oldest, assistant, newest];
    for id in &ids {
        manager.message_store.unload(id).await;
    }

    assert_eq!(manager.list_user_input_history(1).await?, ["newest"]);
    assert_eq!(
        manager.message_store.list_hot().await?,
        ids.into_iter().collect()
    );

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn limited_user_history_rejects_invalid_ancestors_after_the_limit() -> Result<()> {
    for corruption in ["cycle", "missing", "malformed"] {
        let (mut manager, data_dir) = test_manager().await;
        let session_id = manager.create_session().await?;
        let oldest = manager
            .add_message(&session_id, ChatRole::User, "oldest".into(), None)
            .await?;
        let newest = manager
            .add_message(&session_id, ChatRole::User, "newest".into(), None)
            .await?;
        let expected = match corruption {
            "cycle" => {
                let mut message = manager.message_store.get(&oldest).await?.unwrap();
                message.parent_id = Some(newest);
                manager.message_store.save(&message).await?;
                "cycle repeats message"
            }
            "missing" => {
                manager.message_store.delete(&oldest).await?;
                "disappeared while reading session history"
            }
            _ => {
                manager.message_store.unload(&oldest).await;
                tokio::fs::write(
                    data_dir.join("messages").join(format!("{oldest}.json")),
                    "{}",
                )
                .await?;
                "Failed to parse message file"
            }
        };

        let error = manager.list_user_input_history(1).await.unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "{corruption}: {error}"
        );
        assert!(manager.list_user_input_history(0).await?.is_empty());

        cleanup_dir(data_dir).await;
    }
    Ok(())
}

#[tokio::test]
async fn limited_user_history_preserves_validation_at_the_session_boundary() -> Result<()> {
    for has_ancestor in [false, true] {
        let (mut manager, data_dir) = test_manager().await;
        let newest_session = manager.create_session().await?;
        if has_ancestor {
            manager
                .add_message(&newest_session, ChatRole::Assistant, "reply".into(), None)
                .await?;
        }
        manager
            .add_message(&newest_session, ChatRole::User, "newest".into(), None)
            .await?;
        let older_session = manager.create_session().await?;
        let missing = manager
            .add_message(&older_session, ChatRole::User, "older".into(), None)
            .await?;
        manager.message_store.delete(&missing).await?;
        for (session_id, updated_at) in [(newest_session, 2), (older_session, 1)] {
            let mut session = manager.session_store.get(&session_id).await?.unwrap();
            session.updated_at = updated_at;
            manager.session_store.save(&session).await?;
        }

        let history = manager.list_user_input_history(1).await;
        if has_ancestor {
            assert_eq!(history?, ["newest"]);
        } else {
            assert!(
                history
                    .unwrap_err()
                    .to_string()
                    .contains("disappeared while reading session history")
            );
        }

        cleanup_dir(data_dir).await;
    }
    Ok(())
}
