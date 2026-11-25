use super::*;
use kraai_types::{ProviderId, ToolCallId};

#[test]
fn trims_new_tool_output_without_removing_call_or_encrypted_history() {
    let encrypted = ConversationItem::Compaction {
        provider_id: ProviderId::new("codex"),
        payload: serde_json::json!({"type":"compaction","encrypted_content":"x".repeat(1000)}),
    };
    let call = ConversationItem::Assistant {
        items: vec![AssistantItem::ScriptCall(ScriptCall {
            call_id: ToolCallId::new("call"),
            name: "tool".into(),
            input: "inspect".into(),
            arguments_error: None,
            rewrite: None,
        })],
    };
    let mut request = ProviderRequest {
        messages: vec![
            encrypted.clone(),
            call.clone(),
            ConversationItem::ScriptResult {
                outcome: kraai_execution_types::ScriptExecutionOutcome {
                    status: kraai_execution_types::ScriptExecutionStatus::Completed,
                    exit_code: Some(0),
                },
                call_id: ToolCallId::new("call"),
                output: "x".repeat(10000).into(),
            },
            ConversationItem::System {
                text: "pinned file".into(),
            },
        ],
        script_tool: None,
        options: Default::default(),
    };
    trim_tool_outputs(&mut request, Some(1000));
    assert_eq!(request.messages.first(), Some(&encrypted));
    assert_eq!(request.messages.get(1), Some(&call));
    assert!(
        matches!(request.messages.get(2), Some(ConversationItem::ScriptResult { call_id, output, .. }) if call_id.as_str() == "call" && output.as_text() == Some(TRUNCATED_OUTPUT))
    );
}
#[test]
fn truncated_images_remain_reopenable_without_resolving_the_blobs() {
    let images: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|id| kraai_execution_types::ImageAttachment {
            id: id.repeat(64),
            mime_type: "image/png".into(),
            width: 2,
            height: 3,
            byte_length: 100,
        })
        .collect();
    let mut output = MessageContent::from("x".repeat(10000));
    output.0.extend(
        images
            .iter()
            .cloned()
            .map(|image| ContentPart::Image { image }),
    );
    let mut request = ProviderRequest {
        messages: vec![ConversationItem::ScriptResult {
            outcome: kraai_execution_types::ScriptExecutionOutcome {
                status: kraai_execution_types::ScriptExecutionStatus::Completed,
                exit_code: Some(0),
            },
            call_id: ToolCallId::new("call"),
            output,
        }],
        script_tool: None,
        options: Default::default(),
    };
    trim_tool_outputs(&mut request, Some(1000));
    let Some(ConversationItem::ScriptResult { output, .. }) = request.messages.first() else {
        unreachable!("expected script result");
    };
    assert!(!output.has_images());
    let text = output.display_text();
    assert!(text.starts_with(TRUNCATED_OUTPUT));
    for image in images {
        assert!(text.contains(&format!("kraai-view-image --attachment {}", image.id)));
    }
    assert!(text.len() < 4000);
}
