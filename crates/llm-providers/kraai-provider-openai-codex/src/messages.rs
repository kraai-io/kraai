use kraai_types::{AssistantItem, AssistantPhase, ConversationItem, ProviderId, ToolCallId};
use serde::Serialize;

#[derive(Serialize)]
#[serde(untagged)]
pub enum ResponsesRequestItem {
    Reasoning(serde_json::Value),
    Compaction(serde_json::Value),
    Message(ResponsesRequestMessage),
    CustomToolCall(ResponsesCustomToolCall),
    CustomToolCallOutput(ResponsesCustomToolCallOutput),
}

#[derive(Serialize)]
pub struct ResponsesRequestMessage {
    #[serde(rename = "type")]
    kind: &'static str,
    role: &'static str,
    content: [MessageContentItem; 1],
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
    call_id: ToolCallId,
    name: String,
    input: String,
}

#[derive(Serialize)]
pub struct ResponsesCustomToolCallOutput {
    #[serde(rename = "type")]
    kind: &'static str,
    call_id: ToolCallId,
    output: String,
}

pub struct NormalizedResponsesInput {
    pub instructions: String,
    pub input: Vec<ResponsesRequestItem>,
}

pub fn normalize_conversation(
    messages: Vec<ConversationItem>,
    provider_id: &ProviderId,
) -> NormalizedResponsesInput {
    let mut messages = messages.into_iter().peekable();
    let mut instructions: Option<String> = None;
    while matches!(messages.peek(), Some(ConversationItem::System { .. })) {
        if let Some(ConversationItem::System { text }) = messages.next() {
            match &mut instructions {
                Some(instructions) => {
                    instructions.push_str("\n\n");
                    instructions.push_str(&text);
                }
                None => instructions = Some(text),
            }
        }
    }
    let mut input = Vec::new();

    for message in messages {
        match message {
            ConversationItem::Compaction {
                provider_id: source,
                payload,
            } => {
                if source == *provider_id {
                    input.push(ResponsesRequestItem::Compaction(payload));
                }
            }
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
                        AssistantItem::Reasoning {
                            provider_id: source,
                            payload,
                        } => {
                            if source == *provider_id {
                                input.push(ResponsesRequestItem::Reasoning(payload));
                            }
                        }
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
                                call_id,
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
                        call_id,
                        output,
                    },
                ));
            }
        }
    }

    NormalizedResponsesInput {
        instructions: instructions.unwrap_or_default(),
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
        content: [MessageContentItem {
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
        let input = serde_json::to_value(normalize_conversation(history.clone(), &provider).input)
            .expect("input");
        assert_eq!(input.get(0), Some(&payload));
        assert_eq!(
            input.get(1).and_then(|item| item.get("type")),
            Some(&json!("message"))
        );
        let other = normalize_conversation(history.clone(), &ProviderId::new("other"));
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
                            text: String::from("boundary"),
                        },
                        ConversationItem::System {
                            text: String::from("later instructions"),
                        },
                    ])
                    .collect(),
                &ProviderId::new("codex"),
            );
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
            ],
            &ProviderId::new("codex"),
        );

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
