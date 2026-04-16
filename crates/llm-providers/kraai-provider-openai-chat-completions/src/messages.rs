use color_eyre::eyre::Result;
use kraai_types::{ChatMessage, ChatRole};

use crate::wire::RequestMessage;

pub fn normalize_chat_messages(messages: Vec<ChatMessage>) -> Result<Vec<RequestMessage>> {
    messages
        .into_iter()
        .map(|message| {
            if message.role == ChatRole::Tool {
                Ok(RequestMessage {
                    role: String::from("user"),
                    content: format!("[Tool Result]\n{}", message.content),
                })
            } else {
                Ok(RequestMessage {
                    role: role_to_wire(message.role).to_string(),
                    content: message.content,
                })
            }
        })
        .collect()
}

pub fn role_to_wire(role: ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

pub fn role_from_wire(role: &str) -> ChatRole {
    match role {
        "system" => ChatRole::System,
        "assistant" => ChatRole::Assistant,
        "tool" => ChatRole::Tool,
        "user" => ChatRole::User,
        _ => ChatRole::User,
    }
}
