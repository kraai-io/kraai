use kraai_types::{
    AssistantItem, AssistantPhase, MessageGeneration, ModelId, ProviderId, StreamId, TokenUsage,
    ToolCallId,
};

use super::*;
use crate::app::types::OptimisticMessage;

fn message(id: &str, parent: Option<&str>, content: ConversationItem) -> Message {
    Message {
        id: MessageId::new(id),
        parent_id: parent.map(MessageId::new),
        content,
        status: MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
    }
}

fn text_item(text: &str) -> AssistantItem {
    AssistantItem::Text {
        phase: AssistantPhase::FinalAnswer,
        text: text.to_string(),
    }
}

fn call_item(id: &str, input: &str) -> AssistantItem {
    AssistantItem::ScriptCall {
        call_id: ToolCallId::new(id),
        name: String::from("nushell"),
        input: input.to_string(),
    }
}

fn rendered_lines(state: &AppState) -> Vec<String> {
    state
        .chat_render_cache
        .borrow()
        .sections
        .iter()
        .flat_map(|section| section.iter().map(ChatHistory::line_text))
        .collect()
}

#[test]
fn completed_calls_preserve_mixed_text_pending_calls_and_queued_messages() {
    let call = message(
        "call",
        None,
        ConversationItem::Assistant {
            items: vec![
                text_item("before"),
                call_item("completed", "echo hidden-source"),
                text_item("after"),
                call_item("pending", "echo pending-source"),
            ],
        },
    );
    let original_content = call.content.clone();
    let result = message(
        "result",
        Some("call"),
        ConversationItem::ScriptResult {
            call_id: ToolCallId::new("completed"),
            output: String::from(
                "<tool_call_result status=\"completed\" exit_code=\"0\" elapsed_ms=\"125\">\n<stdout>hidden-output</stdout>\n</tool_call_result>",
            ).into(),
        },
    );
    let mut state = AppState {
        chat_history: [(call.id.clone(), call), (result.id.clone(), result)]
            .into_iter()
            .collect(),
        current_tip_id: Some(String::from("result")),
        optimistic_messages: vec![OptimisticMessage {
            local_id: String::from("queued"),
            content: String::from("next question"),
            content_key: String::from("next question"),
            occurrence: 1,
            is_queued: true,
        }],
        ..AppState::default()
    };

    state.refresh_chat_render_cache(100);
    let collapsed = rendered_lines(&state);
    assert!(collapsed.iter().any(|line| line == " • before"));
    assert!(collapsed.iter().any(|line| line == "   after"));
    assert!(collapsed.iter().any(|line| line.contains("pending-source")));
    assert!(
        collapsed
            .iter()
            .any(|line| line == " ❯ next question [queued]")
    );
    assert!(!collapsed.iter().any(|line| line.contains("hidden-source")));
    assert!(!collapsed.iter().any(|line| line.contains("hidden-output")));
    assert_eq!(
        state
            .chat_history
            .get(&MessageId::new("call"))
            .map(|message| &message.content),
        Some(&original_content),
    );

    state
        .execution_expanded
        .insert(String::from("completed"), true);
    state.chat_epoch += 1;
    state.refresh_chat_render_cache(100);
    let expanded = rendered_lines(&state);
    assert!(expanded.iter().any(|line| line.contains("hidden-source")));
    assert!(expanded.iter().any(|line| line.contains("hidden-output")));
    assert!(expanded.iter().any(|line| line.contains("pending-source")));

    state.execution_expanded.clear();
    state.chat_epoch += 1;
    state.refresh_chat_render_cache(100);
    assert_eq!(rendered_lines(&state), collapsed);
}

#[test]
#[expect(
    clippy::expect_used,
    reason = "cache regression test requires rendered entries"
)]
fn streaming_refresh_reuses_unchanged_messages_and_rebuilds_after_resize() {
    let user = message(
        "user",
        None,
        ConversationItem::User {
            content: String::from("question").into(),
        },
    );
    let assistant = message(
        "assistant",
        Some("user"),
        ConversationItem::Assistant {
            items: vec![text_item("first")],
        },
    );
    let mut state = AppState {
        chat_history: [(user.id.clone(), user), (assistant.id.clone(), assistant)]
            .into_iter()
            .collect(),
        current_tip_id: Some(String::from("assistant")),
        ..AppState::default()
    };
    state.refresh_chat_render_cache(80);
    let user_lines = Arc::clone(
        &state
            .chat_render_cache
            .borrow()
            .message_cache
            .get("user")
            .expect("user render")
            .lines,
    );

    state
        .chat_history
        .get_mut(&MessageId::new("assistant"))
        .expect("assistant message")
        .content = ConversationItem::Assistant {
        items: vec![text_item("first second")],
    };
    state.chat_epoch += 1;
    state.refresh_chat_render_cache(80);
    assert!(Arc::ptr_eq(
        &user_lines,
        &state
            .chat_render_cache
            .borrow()
            .message_cache
            .get("user")
            .expect("user render")
            .lines,
    ));
    assert!(
        rendered_lines(&state)
            .iter()
            .any(|line| line == " • first second")
    );

    state.refresh_chat_render_cache(40);
    assert!(!Arc::ptr_eq(
        &user_lines,
        &state
            .chat_render_cache
            .borrow()
            .message_cache
            .get("user")
            .expect("resized user render")
            .lines,
    ));
    assert!(
        rendered_lines(&state)
            .iter()
            .any(|line| line == " • first second")
    );
}

#[test]
fn completed_call_projection_preserves_message_metadata_and_item_order() {
    let mut original = message(
        "assistant",
        Some("parent"),
        ConversationItem::Assistant {
            items: vec![
                text_item("before"),
                call_item("completed", "echo source"),
                text_item(""),
                call_item("pending", "echo pending"),
                text_item("after"),
            ],
        },
    );
    original.agent_profile_id = Some(String::from("profile"));
    original.generation = Some(MessageGeneration {
        provider_id: ProviderId::new("provider"),
        model_id: ModelId::new("model"),
        max_context: Some(4096),
        usage: Some(TokenUsage {
            input_tokens: 123,
            output_tokens: 45,
            ..TokenUsage::default()
        }),
    });
    let completed = HashSet::from(["completed"]);
    for status in [
        MessageStatus::Complete,
        MessageStatus::Streaming {
            stream_id: StreamId::new("stream"),
        },
        MessageStatus::Cancelled,
    ] {
        original.status = status;
        let projected = without_completed_calls(&original, &completed);
        let mut expected = original.clone();
        expected.content = ConversationItem::Assistant {
            items: vec![
                text_item("before"),
                text_item(""),
                call_item("pending", "echo pending"),
                text_item("after"),
            ],
        };
        assert_eq!(projected.id, expected.id);
        assert_eq!(projected.parent_id, expected.parent_id);
        assert_eq!(projected.content, expected.content);
        assert_eq!(projected.status, expected.status);
        assert_eq!(projected.agent_profile_id, expected.agent_profile_id);
        assert_eq!(projected.generation, expected.generation);
        assert_eq!(
            message_fingerprint(&projected),
            message_fingerprint(&expected)
        );
        assert!(matches!(
            without_completed_calls(&original, &HashSet::from(["other"])),
            Cow::Borrowed(_)
        ));
    }
}

#[test]
fn duplicate_completed_call_sources_use_last_item_and_update_cached_result() {
    let first = message(
        "first",
        None,
        ConversationItem::Assistant {
            items: vec![call_item("same-call", "echo first-source")],
        },
    );
    let second = message(
        "second",
        Some("first"),
        ConversationItem::Assistant {
            items: vec![
                call_item("same-call", "echo second-source"),
                call_item("same-call", "echo final-source"),
            ],
        },
    );
    let result = message(
        "result",
        Some("second"),
        ConversationItem::ScriptResult {
            call_id: ToolCallId::new(String::from("same-call")),
            output: String::from("<tool_call_result status=\"completed\" />").into(),
        },
    );
    let mut state = AppState {
        chat_history: [first, second, result]
            .into_iter()
            .map(|message| (message.id.clone(), message))
            .collect(),
        current_tip_id: Some(String::from("result")),
        execution_expanded: HashMap::from([(String::from("same-call"), true)]),
        ..AppState::default()
    };
    state.refresh_chat_render_cache(100);
    let initial = rendered_lines(&state);
    assert!(initial.iter().any(|line| line.contains("final-source")));
    assert!(!initial.iter().any(|line| line.contains("first-source")));
    assert!(!initial.iter().any(|line| line.contains("second-source")));
    assert_eq!(state.chat_render_cache.borrow().sections.len(), 1);
    assert_eq!(
        state.chat_render_cache.borrow().execution_offsets,
        HashMap::from([(String::from("same-call"), 0)]),
    );

    state.chat_history.insert(
        MessageId::new("second"),
        message(
            "second",
            Some("first"),
            ConversationItem::Assistant {
                items: vec![call_item("same-call", "echo replaced-source")],
            },
        ),
    );
    state.chat_epoch += 1;
    state.refresh_chat_render_cache(100);
    let updated = rendered_lines(&state);
    assert!(updated.iter().any(|line| line.contains("replaced-source")));
    assert!(!updated.iter().any(|line| line.contains("final-source")));

    state.mode = UiMode::Executions;
    state.chat_epoch += 1;
    state.refresh_chat_render_cache(100);
    assert_eq!(rendered_lines(&state), updated);
}
