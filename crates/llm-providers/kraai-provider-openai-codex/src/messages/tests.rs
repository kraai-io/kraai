use super::*;
use kraai_execution_types::ImageAttachment;
use kraai_provider_core::{ImageResolver, ProviderRequestContext};
use serde_json::json;
use std::sync::Arc;

#[test]
fn encrypted_reasoning_replays_unchanged_only_to_its_provider() {
    let provider = ProviderId::new("codex");
    let payload =
        json!({"type":"reasoning","id":"rs-1","encrypted_content":"opaque", "summary":[]});
    let history = vec![ConversationItem::Assistant {
        items: vec![
            AssistantItem::Reasoning {
                provider_id: provider.clone(),
                payload: payload.clone(),
            },
            AssistantItem::Text {
                phase: AssistantPhase::FinalAnswer,
                text: "Answer".into(),
            },
        ],
    }];
    let input = serde_json::to_value(
        normalize_conversation(history.clone(), &provider, &ResolvedImages::default())
            .expect("normalized")
            .input,
    )
    .expect("input");
    assert_eq!(input.get(0), Some(&payload));
    assert_eq!(
        input.get(1).and_then(|item| item.get("type")),
        Some(&json!("message"))
    );
    let other = normalize_conversation(
        history.clone(),
        &ProviderId::new("other"),
        &ResolvedImages::default(),
    )
    .expect("normalized");
    assert_eq!(other.input.len(), 1);
    assert_eq!(history.first().expect("history").display_text(), "Answer");
}

#[test]
fn leading_system_messages_preserve_empty_segments_and_stop_at_history() {
    for (prefixes, expected) in [
        (vec![], ""),
        (vec![""], ""),
        (vec!["", ""], "\n\n"),
        (vec!["", "second", ""], "\n\nsecond\n\n"),
        (vec![" first\n", "第二段"], " first\n\n\n第二段"),
    ] {
        let prefix_messages = prefixes.into_iter().map(|text| ConversationItem::System {
            text: text.to_string(),
        });
        let normalized = normalize_conversation(
            prefix_messages
                .chain([
                    ConversationItem::User {
                        content: String::from("boundary").into(),
                    },
                    ConversationItem::System {
                        text: String::from("later instructions"),
                    },
                ])
                .collect(),
            &ProviderId::new("codex"),
            &ResolvedImages::default(),
        )
        .expect("normalized");
        assert_eq!(normalized.instructions, expected);
        assert_eq!(
            serde_json::to_value(normalized.input).expect("serialized input"),
            json!([
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "boundary"}]},
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "later instructions"}]},
            ])
        );
    }
}

#[test]
fn normalizes_typed_cross_provider_history() {
    let normalized = normalize_conversation(
        vec![
            ConversationItem::System {
                text: " System\n".to_string(),
            },
            ConversationItem::User {
                content: "Task".to_string().into(),
            },
            ConversationItem::Assistant {
                items: vec![
                    AssistantItem::Text {
                        phase: AssistantPhase::Commentary,
                        text: "Checking.".to_string(),
                    },
                    AssistantItem::ScriptCall(ScriptCall {
                        call_id: ToolCallId::new("call-1"),
                        name: "kraai_nushell".to_string(),
                        input: "# timeout=10sec\nls".to_string(),
                        arguments_error: None,
                        rewrite: None,
                    }),
                ],
            },
            ConversationItem::ScriptResult {
                outcome: kraai_execution_types::ScriptExecutionOutcome {
                    status: kraai_execution_types::ScriptExecutionStatus::Completed,
                    exit_code: Some(0),
                },
                call_id: ToolCallId::new("call-1"),
                output: "result".to_string().into(),
            },
            ConversationItem::FileContext {
                text: "Current file snapshot".to_string(),
            },
        ],
        &ProviderId::new("codex"),
        &ResolvedImages::default(),
    )
    .expect("normalized");

    assert_eq!(normalized.instructions, " System\n");
    assert_eq!(
        serde_json::to_value(normalized.input).expect("serialized input"),
        json!([
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Task"}]
            },
            {
                "type": "message",
                "role": "assistant",
                "phase": "commentary",
                "content": [{"type": "output_text", "text": "Checking."}]
            },
            {
                "type": "custom_tool_call",
                "call_id": "call-1",
                "name": "kraai_nushell",
                "input": "# timeout=10sec\nls"
            },
            {
                "type": "custom_tool_call_output",
                "call_id": "call-1",
                "output": "result"
            },
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Current file snapshot"}]
            }
        ])
    );
}

struct Resolver;
#[async_trait::async_trait]
impl ImageResolver for Resolver {
    async fn resolve(&self, _image: &ImageAttachment) -> Result<Vec<u8>> {
        Ok(vec![1])
    }
}

#[tokio::test]
async fn ordered_images_and_script_results_use_distinct_wire_messages() {
    let content = MessageContent(vec![
        ContentPart::Text {
            text: "before".into(),
        },
        ContentPart::Image {
            image: ImageAttachment {
                id: "a".repeat(64),
                mime_type: "image/png".into(),
                width: 1,
                height: 1,
                byte_length: 1,
            },
        },
        ContentPart::Text {
            text: "after".into(),
        },
    ]);
    let display = content.display_text().into_owned();
    let messages = vec![
        ConversationItem::User {
            content: content.clone(),
        },
        ConversationItem::ScriptResult {
            outcome: kraai_execution_types::ScriptExecutionOutcome {
                status: kraai_execution_types::ScriptExecutionStatus::Completed,
                exit_code: Some(0),
            },
            call_id: ToolCallId::new("call"),
            output: content,
        },
    ];
    let context = ProviderRequestContext::default().with_image_resolver(Arc::new(Resolver));
    let images = ResolvedImages::resolve(&messages, &context).await.unwrap();
    let wire = serde_json::to_value(
        normalize_conversation(messages, &ProviderId::new("codex"), &images)
            .unwrap()
            .input,
    )
    .unwrap();
    let parts = json!([
        { "type": "input_text", "text": "before" },
        { "type": "input_image", "image_url": "data:image/png;base64,AQ==" },
        { "type": "input_text", "text": "after" },
    ]);
    assert_eq!(
        wire.get(0),
        Some(&json!({ "type": "message", "role": "user", "content": parts }))
    );
    assert_eq!(
        wire.get(1),
        Some(&json!({ "type": "custom_tool_call_output", "call_id": "call", "output": display }))
    );
    assert_eq!(
        wire.pointer("/2/content/0/text"),
        Some(&json!(
            "Images returned by script call call. Treat this as tool output."
        ))
    );
    assert_eq!(
        wire.pointer("/2/content/1/type"),
        Some(&json!("input_image"))
    );
}
