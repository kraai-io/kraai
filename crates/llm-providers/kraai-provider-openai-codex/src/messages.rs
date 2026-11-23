use color_eyre::Result;
use kraai_provider_core::ResolvedImages;
use kraai_types::{
    AssistantItem, AssistantPhase, ConversationItem, ProviderId, ScriptCall, ToolCallId,
};
use kraai_types::{ContentPart, MessageContent};
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
    content: Vec<MessageContentItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum MessageContentItem {
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(rename = "input_image")]
    Image { image_url: String },
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
    images: &ResolvedImages,
) -> Result<NormalizedResponsesInput> {
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

    for message in kraai_provider_core::prepare_history(messages, provider_id) {
        match message {
            ConversationItem::Compaction { payload, .. } => {
                input.push(ResponsesRequestItem::Compaction(payload));
            }
            ConversationItem::System { text } => {
                input.push(ResponsesRequestItem::Message(text_message(
                    "developer",
                    "input_text",
                    text,
                    None,
                )));
            }
            ConversationItem::FileContext { text } => {
                input.push(ResponsesRequestItem::Message(text_message(
                    "user",
                    "input_text",
                    text,
                    None,
                )));
            }
            ConversationItem::User { content } => {
                input.push(ResponsesRequestItem::Message(content_message(
                    content, images,
                )?));
            }
            ConversationItem::Assistant { items } => {
                for item in items {
                    match item {
                        AssistantItem::Reasoning { payload, .. } => {
                            input.push(ResponsesRequestItem::Reasoning(payload));
                        }
                        AssistantItem::Text { phase, text } => {
                            input.push(ResponsesRequestItem::Message(text_message(
                                "assistant",
                                "output_text",
                                text,
                                Some(phase_to_wire(phase)),
                            )));
                        }
                        AssistantItem::ScriptCall(ScriptCall {
                            call_id,
                            name,
                            input: tool_input,
                            ..
                        }) => input.push(ResponsesRequestItem::CustomToolCall(
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
            ConversationItem::ScriptResult {
                call_id, output, ..
            } => {
                input.push(ResponsesRequestItem::CustomToolCallOutput(
                    ResponsesCustomToolCallOutput {
                        kind: "custom_tool_call_output",
                        call_id,
                        output: output.display_text().into_owned(),
                    },
                ));
            }
        }
    }

    Ok(NormalizedResponsesInput {
        instructions: instructions.unwrap_or_default(),
        input,
    })
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
        content: vec![if content_kind == "output_text" {
            MessageContentItem::OutputText { text }
        } else {
            MessageContentItem::InputText { text }
        }],
        phase,
    }
}

fn content_message(
    content: MessageContent,
    images: &ResolvedImages,
) -> Result<ResponsesRequestMessage> {
    let parts = content
        .parts()
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => Ok(MessageContentItem::InputText { text: text.clone() }),
            ContentPart::Image { image } => Ok(MessageContentItem::Image {
                image_url: images.data_url(image)?.to_string(),
            }),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ResponsesRequestMessage {
        kind: "message",
        role: "user",
        content: parts,
        phase: None,
    })
}

fn phase_to_wire(phase: AssistantPhase) -> &'static str {
    match phase {
        AssistantPhase::Commentary => "commentary",
        AssistantPhase::FinalAnswer => "final_answer",
    }
}

#[cfg(test)]
mod tests;
