use kraai_types::{AssistantItem, AssistantPhase, ConversationItem};
use serde::Serialize;

#[derive(Serialize)]
#[serde(untagged)]
pub enum ResponsesRequestItem {
    Message(ResponsesRequestMessage),
    CustomToolCall(ResponsesCustomToolCall),
    CustomToolCallOutput(ResponsesCustomToolCallOutput),
}

#[derive(Serialize)]
pub struct ResponsesRequestMessage {
    #[serde(rename = "type")]
    kind: &'static str,
    role: &'static str,
    content: Vec<MessageContentItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<&'static str>,
}

#[derive(Serialize)]
struct MessageContentItem {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

#[derive(Serialize)]
pub struct ResponsesCustomToolCall {
    #[serde(rename = "type")]
    kind: &'static str,
    call_id: String,
    name: String,
    input: String,
}

#[derive(Serialize)]
pub struct ResponsesCustomToolCallOutput {
    #[serde(rename = "type")]
    kind: &'static str,
    call_id: String,
    output: String,
}

pub struct NormalizedResponsesInput {
    pub instructions: String,
    pub input: Vec<ResponsesRequestItem>,
}

pub fn normalize_conversation(messages: Vec<ConversationItem>) -> NormalizedResponsesInput {
    let mut messages = messages.into_iter().peekable();
    let mut instructions = Vec::new();
    while matches!(messages.peek(), Some(ConversationItem::System { .. })) {
        if let Some(ConversationItem::System { text }) = messages.next() {
            instructions.push(text);
        }
    }
    let mut input = Vec::new();

    for message in messages {
        match message {
            ConversationItem::System { text } => {
                input.push(ResponsesRequestItem::Message(text_message(
                    "developer",
                    "input_text",
                    text,
                    None,
                )));
            }
            ConversationItem::User { text } => {
                input.push(ResponsesRequestItem::Message(text_message(
                    "user",
                    "input_text",
                    text,
                    None,
                )));
            }
            ConversationItem::Assistant { items } => {
                for item in items {
                    match item {
                        AssistantItem::Text { phase, text } => {
                            input.push(ResponsesRequestItem::Message(text_message(
                                "assistant",
                                "output_text",
                                text,
                                Some(phase_to_wire(phase)),
                            )));
                        }
                        AssistantItem::ScriptCall {
                            call_id,
                            name,
                            input: tool_input,
                        } => input.push(ResponsesRequestItem::CustomToolCall(
                            ResponsesCustomToolCall {
                                kind: "custom_tool_call",
                                call_id: call_id.to_string(),
                                name,
                                input: tool_input,
                            },
                        )),
                    }
                }
            }
            ConversationItem::ScriptResult { call_id, output } => {
                input.push(ResponsesRequestItem::CustomToolCallOutput(
                    ResponsesCustomToolCallOutput {
                        kind: "custom_tool_call_output",
                        call_id: call_id.to_string(),
                        output,
                    },
                ));
            }
        }
    }

    NormalizedResponsesInput {
        instructions: instructions.join("\n\n"),
        input,
    }
}

fn text_message(
    role: &'static str,
    content_kind: &'static str,
    text: String,
    phase: Option<&'static str>,
) -> ResponsesRequestMessage {
    ResponsesRequestMessage {
        kind: "message",
        role,
        content: vec![MessageContentItem {
            kind: content_kind,
            text,
        }],
        phase,
    }
}

fn phase_to_wire(phase: AssistantPhase) -> &'static str {
    match phase {
        AssistantPhase::Commentary => "commentary",
        AssistantPhase::FinalAnswer => "final_answer",
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests use direct assertions for serialized wire fixtures"
)]
mod tests {
    use super::*;
    use kraai_types::ToolCallId;
    use serde_json::json;

    #[test]
    fn normalizes_typed_cross_provider_history() {
        let normalized = normalize_conversation(vec![
            ConversationItem::System {
                text: " System\n".to_string(),
            },
            ConversationItem::User {
                text: "Task".to_string(),
            },
            ConversationItem::Assistant {
                items: vec![
                    AssistantItem::Text {
                        phase: AssistantPhase::Commentary,
                        text: "Checking.".to_string(),
                    },
                    AssistantItem::ScriptCall {
                        call_id: ToolCallId::new("call-1"),
                        name: "kraai_nushell".to_string(),
                        input: "# timeout=10sec\nls".to_string(),
                    },
                ],
            },
            ConversationItem::ScriptResult {
                call_id: ToolCallId::new("call-1"),
                output: "result".to_string(),
            },
            ConversationItem::System {
                text: "Current pinned files".to_string(),
            },
        ]);

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
                    "role": "developer",
                    "content": [{"type": "input_text", "text": "Current pinned files"}]
                }
            ])
        );
    }
}
