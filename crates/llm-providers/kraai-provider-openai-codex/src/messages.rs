use color_eyre::eyre::Result;
use kraai_types::{ChatMessage, ChatRole};
use serde::Serialize;

const DEFAULT_CODEX_INSTRUCTIONS: &str = "You are Codex, a coding agent.";

#[derive(Serialize)]
pub struct ResponsesRequestMessage {
    #[serde(rename = "type")]
    kind: &'static str,
    role: &'static str,
    content: Vec<MessageContentItem>,
}

#[derive(Serialize)]
struct MessageContentItem {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

pub struct NormalizedResponsesInput {
    pub instructions: String,
    pub input: Vec<ResponsesRequestMessage>,
}

pub fn normalize_chat_messages(messages: Vec<ChatMessage>) -> Result<NormalizedResponsesInput> {
    let mut instructions = Vec::new();
    let input = messages
        .into_iter()
        .filter_map(|message| {
            if message.role == ChatRole::System {
                let text = message.content.trim();
                if !text.is_empty() {
                    instructions.push(text.to_string());
                }
                return None;
            }

            let role = match message.role {
                ChatRole::System => unreachable!("system messages are extracted to instructions"),
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
                ChatRole::Tool => "user",
            };
            let content_kind = if message.role == ChatRole::Assistant {
                "output_text"
            } else {
                "input_text"
            };
            let text = if message.role == ChatRole::Tool {
                format!("[Tool Result]\n{}", message.content)
            } else {
                message.content
            };

            Some(ResponsesRequestMessage {
                kind: "message",
                role,
                content: vec![MessageContentItem {
                    kind: content_kind,
                    text,
                }],
            })
        })
        .collect();

    let instructions = if instructions.is_empty() {
        DEFAULT_CODEX_INSTRUCTIONS.to_string()
    } else {
        instructions.join("\n\n")
    };

    Ok(NormalizedResponsesInput {
        instructions,
        input,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kraai_types::ChatRole;
    use serde_json::json;

    #[test]
    fn normalize_chat_messages_extracts_system_messages_into_instructions() {
        let normalized = normalize_chat_messages(vec![
            ChatMessage {
                role: ChatRole::System,
                content: "System A".to_string(),
            },
            ChatMessage {
                role: ChatRole::User,
                content: "User".to_string(),
            },
            ChatMessage {
                role: ChatRole::System,
                content: "System B".to_string(),
            },
        ])
        .expect("normalized");

        assert_eq!(normalized.instructions, "System A\n\nSystem B");
        assert_eq!(normalized.input.len(), 1);
    }

    #[test]
    fn normalize_chat_messages_uses_default_instructions_when_missing() {
        let normalized = normalize_chat_messages(vec![ChatMessage {
            role: ChatRole::User,
            content: "User".to_string(),
        }])
        .expect("normalized");

        assert_eq!(normalized.instructions, DEFAULT_CODEX_INSTRUCTIONS);
        assert_eq!(normalized.input.len(), 1);
    }

    #[test]
    fn normalize_chat_messages_uses_output_text_for_assistant_history() {
        let normalized = normalize_chat_messages(vec![
            ChatMessage {
                role: ChatRole::User,
                content: "User".to_string(),
            },
            ChatMessage {
                role: ChatRole::Assistant,
                content: "Assistant".to_string(),
            },
        ])
        .expect("normalized");

        let json = serde_json::to_value(&normalized.input).expect("serialized");
        assert_eq!(
            json,
            json!([
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "User" }]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "Assistant" }]
                }
            ])
        );
    }
}
