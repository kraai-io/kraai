use color_eyre::Result;
use kraai_provider_core::ResolvedImages;
use kraai_types::{ContentPart, ConversationItem, MessageContent, ScriptCall};

use crate::wire::{ImageUrl, RequestContent, RequestContentPart, RequestMessage};

pub fn normalize_chat_messages(
    messages: Vec<ConversationItem>,
    images: &ResolvedImages,
    provider_id: &kraai_types::ProviderId,
) -> Result<Vec<RequestMessage>> {
    let mut normalized = Vec::new();
    for message in kraai_provider_core::prepare_history(messages, provider_id) {
        match message {
            ConversationItem::Compaction { .. } => {}
            ConversationItem::System { text } => normalized.push(RequestMessage::System {
                content: RequestContent::Text(text),
            }),
            ConversationItem::FileContext { text } => normalized.push(RequestMessage::User {
                content: RequestContent::Text(text),
            }),
            ConversationItem::User { content } => normalized.push(RequestMessage::User {
                content: content_parts(content, images)?,
            }),
            ConversationItem::Assistant { items } => {
                let mut text = Vec::new();
                let mut reasoning = crate::reasoning::Reasoning::default();
                let mut tool_calls = Vec::new();
                for item in items {
                    match item {
                        kraai_types::AssistantItem::Reasoning { payload, .. } => {
                            reasoning.append(serde_json::from_value(payload)?)?;
                        }
                        kraai_types::AssistantItem::Text { text: value, .. } => text.push(value),
                        kraai_types::AssistantItem::ScriptCall(ScriptCall {
                            call_id,
                            name,
                            input,
                            arguments_error,
                            ..
                        }) => {
                            // Providers reject a request whose call arguments are not a JSON
                            // object, so a cut off or malformed call keeps its text as the input.
                            let replays_as_sent =
                                arguments_error.is_some()
                                    && serde_json::from_str::<
                                        serde_json::Map<String, serde_json::Value>,
                                    >(&input)
                                    .is_ok();
                            let arguments = if replays_as_sent {
                                input
                            } else {
                                serde_json::to_string(&crate::wire::ScriptArguments { input })?
                            };
                            tool_calls.push(crate::wire::FunctionCall {
                                id: call_id,
                                kind: "function",
                                function: crate::wire::FunctionArguments { name, arguments },
                            });
                        }
                    }
                }
                if !text.is_empty() || !tool_calls.is_empty() {
                    normalized.push(RequestMessage::Assistant {
                        reasoning,
                        content: (!text.is_empty()).then(|| text.join("\n\n")),
                        tool_calls,
                    });
                }
            }
            ConversationItem::ScriptResult {
                call_id, output, ..
            } => {
                normalized.push(RequestMessage::Tool {
                    tool_call_id: call_id,
                    content: RequestContent::Text(output.display_text().into_owned()),
                });
            }
        }
    }
    Ok(normalized)
}

fn content_parts(content: MessageContent, images: &ResolvedImages) -> Result<RequestContent> {
    if !content.has_images() {
        return Ok(RequestContent::Text(content.display_text().into_owned()));
    }
    Ok(RequestContent::Parts(
        content
            .parts()
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => Ok(RequestContentPart::Text {
                    text: text.clone(),
                    cache_control: None,
                }),
                ContentPart::Image { image } => Ok(RequestContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: images.data_url(image)?.to_string(),
                    },
                }),
            })
            .collect::<Result<Vec<_>>>()?,
    ))
}

#[cfg(test)]
mod tests;
