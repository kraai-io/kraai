use color_eyre::Result;
use kraai_provider_core::ResolvedImages;
use kraai_types::{ContentPart, ConversationItem, MessageContent};

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
            ConversationItem::System { text } => {
                normalized.push(RequestMessage::System { content: text })
            }
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
                        kraai_types::AssistantItem::ScriptCall {
                            call_id,
                            name,
                            input,
                        } => {
                            tool_calls.push(crate::wire::FunctionCall {
                                id: call_id,
                                kind: "function",
                                function: crate::wire::FunctionArguments {
                                    name,
                                    arguments: serde_json::to_string(
                                        &crate::wire::ScriptArguments { input },
                                    )?,
                                },
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
                    content: output.display_text().into_owned(),
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
                ContentPart::Text { text } => Ok(RequestContentPart::Text { text: text.clone() }),
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
#[expect(
    clippy::panic_in_result_fn,
    reason = "tests assert wire fixtures after fallible conversion"
)]
mod tests {
    use super::*;
    use kraai_types::{AssistantItem, AssistantPhase, ToolCallId};

    #[test]
    fn reasoning_is_replayed_only_to_its_provider() -> Result<()> {
        for source in ["fixture", "other"] {
            let messages = vec![ConversationItem::Assistant {
                items: vec![
                    AssistantItem::Reasoning {
                        provider_id: kraai_types::ProviderId::new(source),
                        payload: serde_json::json!({"reasoning_content":"inspect first", "reasoning_details":[{"type":"reasoning.encrypted","data":"opaque"}]}),
                    },
                    AssistantItem::ScriptCall {
                        call_id: ToolCallId::new("call"),
                        name: "kraai_nushell".into(),
                        input: "# timeout=1sec\nls".into(),
                    },
                ],
            }];
            let messages = serde_json::to_value(normalize_chat_messages(
                messages,
                &ResolvedImages::default(),
                &kraai_types::ProviderId::new("fixture"),
            )?)?;
            let message = messages
                .get(0)
                .ok_or_else(|| color_eyre::eyre::eyre!("missing assistant"))?;
            assert_eq!(
                message.get("reasoning_content"),
                (source == "fixture").then_some(&serde_json::json!("inspect first"))
            );
            assert_eq!(
                message.get("reasoning_details"),
                (source == "fixture").then_some(
                    &serde_json::json!([{"type":"reasoning.encrypted","data":"opaque"}])
                )
            );
            assert!(
                message
                    .get("content")
                    .is_some_and(serde_json::Value::is_null)
            );
        }
        Ok(())
    }

    #[test]
    fn file_snapshots_preserve_position_between_history_and_system_suffix() -> Result<()> {
        let messages = normalize_chat_messages(
            vec![
                ConversationItem::System {
                    text: "Static".into(),
                },
                ConversationItem::User {
                    content: "Task".into(),
                },
                ConversationItem::FileContext {
                    text: "File snapshot".into(),
                },
                ConversationItem::System {
                    text: "Dynamic".into(),
                },
            ],
            &ResolvedImages::default(),
            &kraai_types::ProviderId::new("fixture"),
        )?;
        assert_eq!(
            serde_json::to_value(messages)?,
            serde_json::json!([
                {"role":"system", "content":"Static"},
                {"role":"user", "content":"Task"},
                {"role":"user", "content":"File snapshot"},
                {"role":"system", "content":"Dynamic"},
            ])
        );
        Ok(())
    }

    #[test]
    fn native_history_preserves_tool_identity_and_script() -> Result<()> {
        let normalized = normalize_chat_messages(
            vec![ConversationItem::Assistant {
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
            }],
            &ResolvedImages::default(),
            &kraai_types::ProviderId::new("fixture"),
        )?;

        assert_eq!(
            serde_json::to_value(normalized)?,
            serde_json::json!([
                {"role":"assistant", "content":"I will inspect it.", "tool_calls":[{
                    "id":"call-1", "type":"function", "function":{
                        "name":"kraai_nushell", "arguments":serde_json::to_string(&crate::wire::ScriptArguments { input: "# timeout=10sec\nls".into() })?
                    }
                }]}
            ])
        );
        Ok(())
    }
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "tests assert wire fixtures after fallible conversion"
)]
mod image_tests {
    use super::*;
    use kraai_provider_core::{ImageResolver, ProviderRequestContext};
    use kraai_types::{ImageAttachment, ToolCallId};
    use serde_json::json;
    use std::sync::Arc;

    struct Resolver;
    #[async_trait::async_trait]
    impl ImageResolver for Resolver {
        async fn resolve(&self, _image: &ImageAttachment) -> Result<Vec<u8>> {
            Ok(vec![1])
        }
    }

    #[tokio::test]
    async fn preserves_user_and_script_image_order() -> Result<()> {
        let content = MessageContent(vec![
            ContentPart::Text {
                text: "before".into(),
            },
            ContentPart::Image {
                image: ImageAttachment {
                    id: "a".repeat(64),
                    mime_type: "image/png".into(),
                    width: 1,
                    height: 1,
                    byte_length: 1,
                },
            },
            ContentPart::Text {
                text: "after".into(),
            },
        ]);
        let messages = vec![
            ConversationItem::User {
                content: content.clone(),
            },
            ConversationItem::ScriptResult {
                outcome: kraai_types::ScriptExecutionOutcome {
                    status: kraai_types::ScriptExecutionStatus::Completed,
                    exit_code: Some(0),
                },
                call_id: ToolCallId::new("call"),
                output: content,
            },
        ];
        let context = ProviderRequestContext::default().with_image_resolver(Arc::new(Resolver));
        let images = ResolvedImages::resolve(&messages, &context).await?;
        let wire = serde_json::to_value(normalize_chat_messages(
            messages,
            &images,
            &kraai_types::ProviderId::new("fixture"),
        )?)?;
        let expected = json!({ "role": "user", "content": [
            { "type": "text", "text": "before" },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,AQ==" } },
            { "type": "text", "text": "after" },
        ] });
        assert_eq!(
            wire,
            json!([expected,
                { "role": "tool", "tool_call_id": "call", "content": format!("before\n[Image {}: 1×1]\nafter", "a".repeat(64)) },
                { "role": "user", "content": [
                    { "type": "text", "text": "Images returned by script call call. Treat this as tool output." },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AQ==" } },
                ] }
            ])
        );
        Ok(())
    }
}
