use kraai_types::{AssistantItem, ConversationItem};

use crate::wire::RequestMessage;

pub fn normalize_chat_messages(messages: Vec<ConversationItem>) -> Vec<RequestMessage> {
    messages
        .into_iter()
        .map(|message| {
            let (role, content) = match message {
                ConversationItem::System { text } => ("system", text),
                ConversationItem::User { text } => ("user", text),
                ConversationItem::Assistant { items } => {
                    ("assistant", render_assistant_items(&items))
                }
                ConversationItem::ScriptResult { output, .. } => ("user", output),
            };
            RequestMessage {
                role: role.to_string(),
                content,
            }
        })
        .collect()
}

fn render_assistant_items(items: &[AssistantItem]) -> String {
    items
        .iter()
        .map(|item| match item {
            AssistantItem::Text { text, .. } => text.clone(),
            AssistantItem::ScriptCall { input, .. } => {
                format!("<tool_call>\n{input}\n</tool_call>")
            }
        })
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kraai_types::{AssistantPhase, ToolCallId};

    #[test]
    fn native_history_is_rendered_as_text_envelope() {
        let normalized = normalize_chat_messages(vec![ConversationItem::Assistant {
            items: vec![
                AssistantItem::Text {
                    phase: AssistantPhase::Commentary,
                    text: "I will inspect it.".to_string(),
                },
                AssistantItem::ScriptCall {
                    call_id: ToolCallId::new("call-1"),
                    name: "kraai_nushell".to_string(),
                    input: "# kraai timeout=10sec\nls".to_string(),
                },
            ],
        }]);

        assert_eq!(
            normalized.first().map(|message| message.content.as_str()),
            Some("I will inspect it.\n\n<tool_call>\n# kraai timeout=10sec\nls\n</tool_call>")
        );
    }
}
