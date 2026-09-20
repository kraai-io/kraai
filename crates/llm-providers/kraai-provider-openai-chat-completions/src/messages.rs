use kraai_types::ConversationItem;

use crate::wire::RequestMessage;

pub fn normalize_chat_messages(messages: Vec<ConversationItem>) -> Vec<RequestMessage> {
    messages
        .into_iter()
        .map(|message| {
            let (role, content) = match message {
                ConversationItem::System { text } => ("system", text),
                ConversationItem::User { text } => ("user", text),
                message @ ConversationItem::Assistant { .. } => {
                    ("assistant", message.display_text().into_owned())
                }
                ConversationItem::ScriptResult { output, .. } => ("user", output),
            };
            RequestMessage { role, content }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kraai_types::{AssistantItem, AssistantPhase, ToolCallId};

    #[test]
    fn system_prefix_and_suffix_stay_around_history() {
        let messages = normalize_chat_messages(vec![
            ConversationItem::System {
                text: "Static".into(),
            },
            ConversationItem::User {
                text: "Task".into(),
            },
            ConversationItem::System {
                text: "Dynamic".into(),
            },
        ]);
        let roles_and_text: Vec<_> = messages
            .iter()
            .map(|message| (message.role, message.content.as_str()))
            .collect();
        assert_eq!(
            roles_and_text,
            vec![
                ("system", "Static"),
                ("user", "Task"),
                ("system", "Dynamic")
            ]
        );
    }

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
                    input: "# timeout=10sec\nls".to_string(),
                },
            ],
        }]);

        assert_eq!(
            normalized.first().map(|message| message.content.as_str()),
            Some("I will inspect it.\n\n<tool_call>\n# timeout=10sec\nls\n</tool_call>")
        );
    }
}
