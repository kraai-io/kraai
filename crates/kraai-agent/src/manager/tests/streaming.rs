use super::super::*;
use super::common::{cleanup_dir, test_manager};
use kraai_types::ScriptCall;

#[tokio::test]
async fn encrypted_reasoning_survives_persistence_and_next_request() {
    use kraai_persistence::MessageStore;
    let (manager, data_dir) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let provider = ProviderId::new("mock");
    let request = manager
        .prepare_start_stream(
            &session,
            "start".into(),
            ModelId::new("mock-model"),
            provider.clone(),
            Default::default(),
        )
        .await
        .unwrap();
    let payload = serde_json::json!({"type":"reasoning","id":"rs-1","encrypted_content":"opaque","summary":[]});
    manager
        .append_reasoning(&request.message_id, provider.clone(), payload.clone())
        .await
        .expect("reasoning");
    let visible = manager
        .append_text_chunk(
            &request.message_id,
            "answer",
            AssistantPhase::FinalAnswer,
            "Answer",
        )
        .await;
    assert_eq!(
        visible,
        Some(StreamedItem::Started {
            index: 1,
            item: AssistantItem::Text {
                phase: AssistantPhase::FinalAnswer,
                text: String::from("Answer"),
            },
        })
    );
    manager.complete_message(&request.message_id).await.unwrap();
    manager.clear_active_turn(&session);
    let stored = kraai_persistence::SqliteMessageStore::new(&data_dir)
        .get(&request.message_id)
        .await
        .unwrap()
        .expect("saved message");
    assert!(
        matches!(stored.content.assistant_items().and_then(|items| items.first()), Some(AssistantItem::Reasoning { payload: actual, .. }) if actual == &payload)
    );
    assert_eq!(stored.content.display_text(), "Answer");
    let next = manager
        .prepare_start_stream(
            &session,
            "continue".into(),
            ModelId::new("mock-model"),
            provider,
            Default::default(),
        )
        .await
        .unwrap();
    assert!(next.provider_request.messages.contains(&stored.content));
    manager
        .abort_streaming_message(&next.message_id)
        .await
        .unwrap();
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn cancelling_reasoning_only_stream_does_not_leave_an_orphan() {
    let (manager, data_dir) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let request = manager
        .prepare_start_stream(
            &session,
            "start".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .append_reasoning(
            &request.message_id,
            ProviderId::new("mock"),
            serde_json::json!({"type":"reasoning","id":"rs-1","encrypted_content":"opaque"}),
        )
        .await
        .expect("reasoning");
    manager
        .cancel_streaming_message(&request.message_id)
        .await
        .unwrap();
    assert!(
        !manager
            .get_chat_history(&session)
            .await
            .unwrap()
            .contains_key(&request.message_id)
    );
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn failed_finalization_preserves_stream_content_and_allows_retry() {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("start").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .append_text_chunk(
            &request.message_id,
            "answer",
            AssistantPhase::FinalAnswer,
            "partial",
        )
        .await
        .expect("text should be appended");
    let original = manager
        .get_chat_history(&session_id)
        .await
        .unwrap()
        .remove(&request.message_id)
        .expect("streaming message");
    let blocked_root = data_dir.join("blocked-storage");
    tokio::fs::write(&blocked_root, b"not a directory")
        .await
        .unwrap();
    let working_store = std::mem::replace(
        &mut manager.message_store,
        Arc::new(kraai_persistence::SqliteMessageStore::new(&blocked_root)),
    );

    assert!(manager.complete_message(&request.message_id).await.is_err());
    assert!(
        manager
            .cancel_streaming_message(&request.message_id)
            .await
            .is_err()
    );
    manager.message_store = working_store;
    let restored = manager
        .get_chat_history(&session_id)
        .await
        .unwrap()
        .remove(&request.message_id)
        .expect("restored streaming message");
    assert_eq!(restored.status, original.status);
    assert_eq!(restored.content, original.content);
    assert_eq!(restored.generation, original.generation);
    assert_eq!(restored.parent_id, original.parent_id);
    assert_eq!(
        super::common::streamed_text(
            manager
                .append_text_chunk(
                    &request.message_id,
                    "answer",
                    AssistantPhase::FinalAnswer,
                    " output",
                )
                .await
        )
        .as_deref(),
        Some(" output")
    );
    manager.complete_message(&request.message_id).await.unwrap();
    let saved = manager
        .message_store
        .get(&request.message_id)
        .await
        .unwrap()
        .expect("completed message");
    assert_eq!(saved.status, MessageStatus::Complete);
    assert_eq!(
        saved.content.assistant_items(),
        Some(
            [AssistantItem::Text {
                phase: AssistantPhase::FinalAnswer,
                text: String::from("partial output"),
            }]
            .as_slice()
        )
    );
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn intercepted_messages_are_rolled_back_when_stream_is_active() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let first = manager
        .prepare_start_stream(
            &session_id,
            String::from("first").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    let messages = vec!["queued one".into(), "queued two".into()];
    let result = manager
        .prepare_messages_stream(
            &session_id,
            messages.clone(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    assert!(result.is_none());
    assert_eq!(
        manager.get_tip(&session_id).await.unwrap(),
        Some(first.message_id.clone())
    );
    manager.complete_message(&first.message_id).await.unwrap();
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
        .expect("retry should start a stream");
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
    assert_eq!(users, vec!["first", "queued one", "queued two"]);
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn duplicate_continuation_trigger_is_ignored_while_stream_is_active() {
    let (manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await.unwrap();
    manager
        .set_session_profile(&session_id, String::from("coding"))
        .await
        .unwrap();

    let first_request = manager
        .prepare_start_stream(
            &session_id,
            String::from("first").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .complete_message(&first_request.message_id)
        .await
        .unwrap();

    let continuation = manager
        .prepare_continuation_stream(&session_id)
        .await
        .unwrap()
        .expect("continuation request should exist");

    let duplicate = manager
        .prepare_continuation_stream(&session_id)
        .await
        .unwrap();
    assert!(duplicate.is_none());
    assert!(manager.is_turn_active(&session_id));

    manager
        .complete_message(&continuation.message_id)
        .await
        .unwrap();

    let next_continuation = manager
        .prepare_continuation_stream(&session_id)
        .await
        .unwrap()
        .expect("session should continue working after duplicate trigger");
    assert_ne!(next_continuation.message_id, continuation.message_id);

    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn prepare_continuation_restarts_a_new_turn_after_previous_turn_is_cleared() {
    let (manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await.unwrap();
    manager
        .set_session_profile(&session_id, String::from("coding"))
        .await
        .unwrap();

    let first_request = manager
        .prepare_start_stream(
            &session_id,
            String::from("first").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .complete_message(&first_request.message_id)
        .await
        .unwrap();
    manager.clear_active_turn(&session_id);

    let continuation = manager
        .prepare_continuation_stream(&session_id)
        .await
        .unwrap()
        .expect("continuation request should exist after clearing the previous turn");

    assert!(manager.is_turn_active(&session_id));
    assert_ne!(continuation.message_id, first_request.message_id);

    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn intercepted_messages_are_batched_before_one_generation() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();

    let first = manager
        .prepare_start_stream(
            &session_id,
            String::from("first").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    let call_id = ToolCallId::new("call-1");
    manager
        .append_script_call(
            &first.message_id,
            ScriptCall {
                call_id: call_id.clone(),
                name: String::from("kraai_nushell"),
                input: String::from("# timeout: 1\n42"),
                arguments_error: None,
                rewrite: None,
            },
        )
        .await
        .expect("script call should be appended");
    manager.complete_message(&first.message_id).await.unwrap();
    manager
        .add_script_result_to_history(
            &session_id,
            MessageId::new(Ulid::generate()),
            String::from("coding"),
            call_id,
            String::from("42").into(),
            kraai_execution_types::ScriptExecutionOutcome {
                status: kraai_execution_types::ScriptExecutionStatus::Completed,
                exit_code: Some(0),
            },
        )
        .await
        .unwrap();

    let intercepted = manager
        .prepare_messages_stream(
            &session_id,
            vec!["queued one".into(), "queued two".into()],
            ModelId::new("new-model"),
            ProviderId::new("mock-alternate"),
            Default::default(),
        )
        .await
        .unwrap()
        .expect("intercepted messages should start one generation");

    assert_eq!(intercepted.model_id, ModelId::new("new-model"));
    assert_eq!(intercepted.provider_id, ProviderId::new("mock-alternate"));
    let users: Vec<_> = intercepted
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
    assert_eq!(users, vec!["first", "queued one", "queued two"]);
    let queued_one_index = intercepted
        .provider_request
        .messages
        .iter()
        .position(|item| matches!(item, ConversationItem::User { content: text } if text.display_text() == "queued one"))
        .expect("first queued message");
    assert!(matches!(
        intercepted
            .provider_request
            .messages
            .get(queued_one_index.saturating_sub(1)),
        Some(ConversationItem::ScriptResult { output, .. }) if output.display_text() == "42"
    ));

    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn cancelled_output_remains_semantic_when_the_next_turn_switches_provider() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();

    let first = manager
        .prepare_start_stream(
            &session_id,
            String::from("start with the first provider").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        super::common::streamed_text(
            manager
                .append_text_chunk(
                    &first.message_id,
                    "commentary-1",
                    AssistantPhase::Commentary,
                    "I am partway through.",
                )
                .await
        )
        .as_deref(),
        Some("I am partway through.")
    );
    manager
        .cancel_streaming_message(&first.message_id)
        .await
        .unwrap();
    manager.clear_active_turn(&session_id);

    let second = manager
        .prepare_start_stream(
            &session_id,
            String::from("continue with a different provider").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock-alternate"),
            Default::default(),
        )
        .await
        .unwrap();

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
}

#[tokio::test]
async fn cancelling_after_a_failed_completion_drops_the_unrun_call() {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("start").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .append_text_chunk(
            &request.message_id,
            "answer",
            AssistantPhase::Commentary,
            "checking",
        )
        .await
        .expect("text should be appended");
    manager
        .append_script_call(
            &request.message_id,
            ScriptCall {
                call_id: ToolCallId::new("call-1"),
                name: String::from("kraai_nushell"),
                input: String::from("ls"),
                arguments_error: None,
                rewrite: None,
            },
        )
        .await
        .expect("script call should be appended");
    let blocked_root = data_dir.join("blocked-storage");
    tokio::fs::write(&blocked_root, b"not a directory")
        .await
        .unwrap();
    let working_store = std::mem::replace(
        &mut manager.message_store,
        Arc::new(kraai_persistence::SqliteMessageStore::new(&blocked_root)),
    );
    assert!(manager.complete_message(&request.message_id).await.is_err());
    manager.message_store = working_store;

    manager
        .cancel_streaming_message(&request.message_id)
        .await
        .unwrap();
    let saved = manager
        .message_store
        .get(&request.message_id)
        .await
        .unwrap()
        .expect("cancelled message");
    assert_eq!(
        saved.content.assistant_items(),
        Some(
            [AssistantItem::Text {
                phase: AssistantPhase::Commentary,
                text: String::from("checking"),
            }]
            .as_slice()
        )
    );
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn failed_streams_keep_text_without_script_calls() {
    let (manager, data_dir) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let request = manager
        .prepare_start_stream(
            &session,
            "start".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .append_text_chunk(
            &request.message_id,
            "answer",
            AssistantPhase::FinalAnswer,
            "partial",
        )
        .await
        .expect("text");
    manager
        .append_script_call(
            &request.message_id,
            ScriptCall {
                call_id: ToolCallId::new("call"),
                name: String::from("kraai_nushell"),
                input: String::from("print partial"),
                arguments_error: None,
                rewrite: None,
            },
        )
        .await
        .expect("call");
    let failed = manager
        .fail_streaming_message(&request.message_id)
        .await
        .unwrap()
        .expect("streaming message");
    assert!(failed.persisted);
    let saved = manager
        .message_store
        .get(&request.message_id)
        .await
        .unwrap()
        .expect("kept message");
    assert_eq!(saved.status, MessageStatus::Cancelled);
    assert_eq!(
        saved.content,
        ConversationItem::Assistant {
            items: vec![AssistantItem::Text {
                phase: AssistantPhase::FinalAnswer,
                text: String::from("partial"),
            }],
        }
    );
    cleanup_dir(data_dir).await;
}
