use color_eyre::Result;
use kraai_provider_core::ResolvedImages;
use kraai_types::{ContentPart, ConversationItem, MessageContent};

use crate::wire::{ImageUrl, RequestContent, RequestContentPart, RequestMessage};

pub fn normalize_chat_messages(
    messages: Vec<ConversationItem>,
    images: &ResolvedImages,
) -> Result<Vec<RequestMessage>> {
    messages
        .into_iter()
        .filter_map(|message| {
            let (role, content) = match message {
                ConversationItem::Compaction { .. } => return None,
                ConversationItem::System { text } => ("system", Ok(RequestContent::Text(text))),
                ConversationItem::User { content } => ("user", content_parts(content, images)),
                message @ ConversationItem::Assistant { .. } => (
                    "assistant",
                    Ok(RequestContent::Text(message.display_text().into_owned())),
                ),
                ConversationItem::ScriptResult { output, .. } => {
                    ("user", content_parts(output, images))
                }
            };
            Some(content.map(|content| RequestMessage { role, content }))
        })
        .collect()
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
    fn system_prefix_and_suffix_stay_around_history() -> Result<()> {
        let messages = normalize_chat_messages(
            vec![
                ConversationItem::System {
                    text: "Static".into(),
                },
                ConversationItem::User {
                    content: "Task".into(),
                },
                ConversationItem::System {
                    text: "Dynamic".into(),
                },
            ],
            &ResolvedImages::default(),
        )?;
        let roles_and_text: Vec<_> = messages
            .iter()
            .map(|message| {
                (
                    message.role,
                    match &message.content {
                        RequestContent::Text(text) => text.as_str(),
                        RequestContent::Parts(_) => "unexpected image",
                    },
                )
            })
            .collect();
        assert_eq!(
            roles_and_text,
            vec![
                ("system", "Static"),
                ("user", "Task"),
                ("system", "Dynamic")
            ]
        );
        Ok(())
    }

    #[test]
    fn native_history_is_rendered_as_text_envelope() -> Result<()> {
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
        )?;

        assert_eq!(
            normalized.first().map(|message| match &message.content {
                RequestContent::Text(text) => text.as_str(),
                RequestContent::Parts(_) => "unexpected image",
            }),
            Some("I will inspect it.\n\n<tool_call>\n# timeout=10sec\nls\n</tool_call>")
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
                call_id: ToolCallId::new("call"),
                output: content,
            },
        ];
        let context = ProviderRequestContext::default().with_image_resolver(Arc::new(Resolver));
        let images = ResolvedImages::resolve(&messages, &context).await?;
        let wire = serde_json::to_value(normalize_chat_messages(messages, &images)?)?;
        let expected = json!({ "role": "user", "content": [
            { "type": "text", "text": "before" },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,AQ==" } },
            { "type": "text", "text": "after" },
        ] });
        assert_eq!(wire, json!([expected, expected]));
        Ok(())
    }
}
