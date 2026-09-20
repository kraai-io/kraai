use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use color_eyre::eyre::{Result, eyre};
use kraai_persistence::MessageStore;

use super::super::*;
use super::common::{cleanup_dir, test_manager};

#[tokio::test]
async fn restart_after_cancelled_result_save_failure_recovers_the_interrupted_tip() -> Result<()> {
    restart_after_cancellation_failure(false, false).await
}

#[tokio::test]
async fn restart_after_cancelled_assistant_completion_failure_preserves_the_pair() -> Result<()> {
    restart_after_cancellation_failure(true, false).await
}

#[tokio::test]
async fn cancelled_assistant_completion_failure_retries_without_duplicate_results() -> Result<()> {
    restart_after_cancellation_failure(true, true).await
}

async fn restart_after_cancellation_failure(
    fail_assistant_completion: bool,
    retry: bool,
) -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let failing = Arc::new(FailingCancellationStore {
        inner: manager.message_store.clone(),
        fail: AtomicBool::new(true),
        fail_assistant_completion,
    });
    manager.message_store = failing.clone();
    manager.conversation_store = ConversationStore::new(failing, manager.session_store.clone());
    let session_id = manager.create_session().await?;
    let request = manager
        .prepare_start_stream(
            &session_id,
            "first".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager
        .append_text_chunk(
            &request.message_id,
            "text",
            AssistantPhase::Commentary,
            "partial text",
        )
        .await;
    manager
        .append_script_call(
            &request.message_id,
            ToolCallId::new("cancelled-call"),
            "kraai_nushell".into(),
            "echo text".into(),
        )
        .await;
    let usage = TokenUsage {
        total_tokens: 15,
        input_tokens: 10,
        output_tokens: 5,
        ..Default::default()
    };
    manager
        .set_streaming_message_usage(&request.message_id, usage.clone())
        .await?;
    assert!(
        manager
            .cancel_streaming_message(&request.message_id, "cancelled before execution")
            .await
            .is_err()
    );
    if retry {
        assert!(
            manager
                .cancel_streaming_message(&request.message_id, "cancelled before execution")
                .await?
                .is_some()
        );
    }
    let providers = manager.cloned_provider_manager();
    drop(manager);

    let (messages, sessions, _, context) = kraai_persistence::init_at(&data_dir).await?;
    let mut reopened = AgentManager::new(
        providers,
        "/tmp/default-workspace".into(),
        messages.clone(),
        sessions,
        context,
        Arc::new(kraai_persistence::FileRequestUsageStore::new(&data_dir)),
        data_dir.clone(),
    );
    assert!(reopened.prepare_session(&session_id).await?);
    let history = reopened.get_chat_history(&session_id).await?;
    if fail_assistant_completion {
        let assistant = history
            .get(&request.message_id)
            .expect("paired assistant must survive restart");
        assert_eq!(assistant.status, MessageStatus::Complete);
        assert_eq!(
            reopened
                .get_session_context_usage(&session_id)
                .await?
                .expect("cancelled assistant usage")
                .usage,
            usage
        );
        assert!(assistant.content.assistant_items().expect("assistant items").iter().any(|item| {
            matches!(item, AssistantItem::Text { phase: AssistantPhase::Commentary, text } if text == "partial text")
        }));
        assert_eq!(
            history
                .values()
                .filter(|message| matches!(&message.content,
            ConversationItem::ScriptResult { call_id, .. } if call_id.as_str() == "cancelled-call"))
                .count(),
            1
        );
    } else {
        assert!(!history.values().any(|message| matches!(&message.content,
            ConversationItem::Assistant { items } if items.iter().any(|item| matches!(item, AssistantItem::ScriptCall { .. })))),
            "restart retained an unpaired cancelled call");
    }
    let next = reopened
        .prepare_start_stream(
            &session_id,
            "next".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    let calls = next
        .provider_request
        .messages
        .iter()
        .filter_map(|item| match item {
            ConversationItem::Assistant { items } => items.iter().find_map(|item| match item {
                AssistantItem::ScriptCall { call_id, .. } => Some(call_id),
                AssistantItem::Text { .. } => None,
            }),
            _ => None,
        });
    for call in calls {
        assert!(
            next.provider_request
                .messages
                .iter()
                .any(|item| matches!(item,
            ConversationItem::ScriptResult { call_id, .. } if call_id == call))
        );
    }
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn script_result_recovery_requires_a_matching_direct_parent_and_no_live_stream() -> Result<()>
{
    for (live, matching, direct) in [
        (true, true, true),
        (false, false, true),
        (false, true, false),
    ] {
        let (mut manager, data_dir) = test_manager().await;
        let session_id = manager.create_session().await?;
        let request = manager
            .prepare_start_stream(
                &session_id,
                "first".into(),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await?;
        manager
            .append_script_call(
                &request.message_id,
                ToolCallId::new("call"),
                "kraai_nushell".into(),
                "echo text".into(),
            )
            .await;
        let source = manager
            .streaming_messages
            .read()
            .await
            .get(&request.message_id)
            .expect("live source")
            .message
            .clone();
        manager.message_store.save(&source).await?;
        if !direct {
            manager
                .append_message(
                    &session_id,
                    ChatRole::Assistant,
                    "intermediate".into(),
                    None,
                )
                .await?;
        }
        manager
            .add_script_result_to_history(
                &session_id,
                MessageId::new(Ulid::generate()),
                "plan".into(),
                ToolCallId::new(if matching { "call" } else { "different-call" }),
                "cancelled".into(),
            )
            .await?;
        if !live {
            manager.streaming_messages.write().await.clear();
        }
        assert!(manager.prepare_session(&session_id).await?);
        assert!(matches!(
            manager
                .message_store
                .get(&request.message_id)
                .await?
                .expect("source remains")
                .status,
            MessageStatus::Streaming { .. }
        ));
        cleanup_dir(data_dir).await;
    }
    Ok(())
}

struct FailingCancellationStore {
    inner: Arc<dyn MessageStore>,
    fail: AtomicBool,
    fail_assistant_completion: bool,
}

#[async_trait::async_trait]
impl MessageStore for FailingCancellationStore {
    async fn save(&self, message: &Message) -> Result<()> {
        let target = if self.fail_assistant_completion {
            message.status == MessageStatus::Complete
                && matches!(&message.content,
                ConversationItem::Assistant { items } if items.iter().any(|item| matches!(item, AssistantItem::ScriptCall { .. })))
        } else {
            matches!(message.content, ConversationItem::ScriptResult { .. })
        };
        if target && self.fail.swap(false, Ordering::SeqCst) {
            return Err(eyre!("injected cancellation persistence failure"));
        }
        self.inner.save(message).await
    }
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        self.inner.get(id).await
    }
    async fn unload(&self, id: &MessageId) {
        self.inner.unload(id).await;
    }
    async fn delete(&self, id: &MessageId) -> Result<()> {
        self.inner.delete(id).await
    }
    async fn exists(&self, id: &MessageId) -> Result<bool> {
        self.inner.exists(id).await
    }
    async fn list_all_on_disk(&self) -> Result<HashSet<MessageId>> {
        self.inner.list_all_on_disk().await
    }
    async fn list_hot(&self) -> Result<HashSet<MessageId>> {
        self.inner.list_hot().await
    }
}
