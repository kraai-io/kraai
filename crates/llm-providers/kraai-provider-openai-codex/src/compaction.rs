use kraai_provider_core::ProviderRequest;
use kraai_types::{AssistantItem, ContentPart, ConversationItem, MessageContent};

const TRUNCATED_OUTPUT: &str = "Output exceeded the available model context and was truncated";

pub(crate) fn trim_tool_outputs(request: &mut ProviderRequest, context_window: Option<usize>) {
    let Some(limit) = context_window else { return };
    let mut bytes = request
        .messages
        .iter()
        .map(visible_bytes)
        .fold(0usize, usize::saturating_add);
    for item in request.messages.iter_mut().rev() {
        if bytes.div_ceil(4) <= limit {
            break;
        }
        if matches!(item, ConversationItem::System { .. }) {
            continue;
        }
        let ConversationItem::ScriptResult { output, .. } = item else {
            break;
        };
        let mut truncated = MessageContent::from(TRUNCATED_OUTPUT);
        truncated
            .0
            .extend(output.images().map(ContentPart::omitted_image));
        bytes = bytes
            .saturating_sub(output.display_text().len())
            .saturating_add(truncated.display_text().len());
        *output = truncated;
    }
}

fn encrypted_bytes(payload: &serde_json::Value) -> usize {
    payload
        .get("encrypted_content")
        .and_then(serde_json::Value::as_str)
        .map_or(0, |text| {
            text.len()
                .saturating_mul(3)
                .saturating_div(4)
                .saturating_sub(650)
        })
}

fn visible_bytes(item: &ConversationItem) -> usize {
    match item {
        ConversationItem::System { text } => text.len(),
        ConversationItem::User { content } => content.display_text().len(),
        ConversationItem::Compaction { payload, .. } => encrypted_bytes(payload),
        ConversationItem::ScriptResult { output, .. } => output.display_text().len(),
        ConversationItem::Assistant { items } => items
            .iter()
            .map(|item| match item {
                AssistantItem::Text { text, .. } => text.len(),
                AssistantItem::Reasoning { payload, .. } => encrypted_bytes(payload),
                AssistantItem::ScriptCall { name, input, .. } => {
                    name.len().saturating_add(input.len())
                }
            })
            .fold(0usize, usize::saturating_add),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kraai_types::{ProviderId, ToolCallId};

    #[test]
    fn trims_new_tool_output_without_removing_call_or_encrypted_history() {
        let encrypted = ConversationItem::Compaction {
            provider_id: ProviderId::new("codex"),
            payload: serde_json::json!({"type":"compaction","encrypted_content":"x".repeat(1000)}),
        };
        let call = ConversationItem::Assistant {
            items: vec![AssistantItem::ScriptCall {
                call_id: ToolCallId::new("call"),
                name: "tool".into(),
                input: "inspect".into(),
            }],
        };
        let mut request = ProviderRequest {
            messages: vec![
                encrypted.clone(),
                call.clone(),
                ConversationItem::ScriptResult {
                    call_id: ToolCallId::new("call"),
                    output: "x".repeat(10000).into(),
                },
                ConversationItem::System {
                    text: "pinned file".into(),
                },
            ],
            script_tool: None,
            cacheable_messages: None,
        };
        trim_tool_outputs(&mut request, Some(1000));
        assert_eq!(request.messages.first(), Some(&encrypted));
        assert_eq!(request.messages.get(1), Some(&call));
        assert!(
            matches!(request.messages.get(2), Some(ConversationItem::ScriptResult { call_id, output }) if call_id.as_str() == "call" && output.as_text() == Some(TRUNCATED_OUTPUT))
        );
    }
    #[test]
    fn truncated_images_remain_reopenable_without_resolving_the_blobs() {
        let images: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|id| kraai_types::ImageAttachment {
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
                call_id: ToolCallId::new("call"),
                output,
            }],
            script_tool: None,
            cacheable_messages: None,
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
}
