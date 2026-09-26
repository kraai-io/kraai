use crate::{ChatRole, ImageAttachment, ProviderId, ToolCallId};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Image { image: ImageAttachment },
}

impl ContentPart {
    pub fn omitted_image(image: &ImageAttachment) -> Self {
        Self::Text {
            text: format!(
                "[Image {} omitted from active context. Reopen with kraai-view-image --attachment {}]",
                image.id, image.id
            ),
        }
    }
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageContent(pub Vec<ContentPart>);

impl From<String> for MessageContent {
    fn from(text: String) -> Self {
        Self(vec![ContentPart::Text { text }])
    }
}

impl From<&str> for MessageContent {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}

impl MessageContent {
    pub fn parts(&self) -> &[ContentPart] {
        &self.0
    }

    pub fn images(&self) -> impl Iterator<Item = &ImageAttachment> {
        self.0.iter().filter_map(|part| match part {
            ContentPart::Image { image } => Some(image),
            ContentPart::Text { .. } => None,
        })
    }

    pub fn has_images(&self) -> bool {
        self.images().next().is_some()
    }

    pub fn as_text(&self) -> Option<&str> {
        match self.0.as_slice() {
            [ContentPart::Text { text }] => Some(text),
            [] => Some(""),
            _ => None,
        }
    }

    pub fn display_text(&self) -> Cow<'_, str> {
        if let Some(text) = self.as_text() {
            return Cow::Borrowed(text);
        }
        Cow::Owned(
            self.0
                .iter()
                .map(|part| match part {
                    ContentPart::Text { text } => text.clone(),
                    ContentPart::Image { image } => {
                        format!("[Image {}: {}×{}]", image.id, image.width, image.height)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    pub fn text_only(&self) -> Cow<'_, str> {
        if let Some(text) = self.as_text() {
            return Cow::Borrowed(text);
        }
        Cow::Owned(
            self.0
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.as_str()),
                    ContentPart::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    pub fn without_images(&self) -> Self {
        Self(
            self.0
                .iter()
                .map(|part| match part {
                    ContentPart::Text { .. } => part.clone(),
                    ContentPart::Image { image } => ContentPart::omitted_image(image),
                })
                .collect(),
        )
    }
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantPhase {
    Commentary,
    FinalAnswer,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantItem {
    /// Opaque provider data retained in history and snapshots, not display text.
    Reasoning {
        provider_id: ProviderId,
        #[cfg_attr(feature = "typescript", ts(type = "unknown"))]
        payload: serde_json::Value,
    },
    Text {
        phase: AssistantPhase,
        text: String,
    },
    ScriptCall {
        call_id: ToolCallId,
        name: String,
        input: String,
    },
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationItem {
    Compaction {
        provider_id: ProviderId,
        #[cfg_attr(feature = "typescript", ts(type = "unknown"))]
        payload: serde_json::Value,
    },
    System {
        text: String,
    },
    User {
        content: MessageContent,
    },
    Assistant {
        items: Vec<AssistantItem>,
    },
    ScriptResult {
        call_id: ToolCallId,
        output: MessageContent,
    },
}

impl ConversationItem {
    pub fn role(&self) -> ChatRole {
        match self {
            Self::System { .. } => ChatRole::System,
            Self::User { .. } => ChatRole::User,
            Self::Assistant { .. } | Self::Compaction { .. } => ChatRole::Assistant,
            Self::ScriptResult { .. } => ChatRole::ToolCallResult,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Self::System { text } => Some(text),
            Self::User { content } => content.as_text(),
            Self::Assistant { .. } | Self::ScriptResult { .. } | Self::Compaction { .. } => None,
        }
    }

    pub fn assistant_items(&self) -> Option<&[AssistantItem]> {
        match self {
            Self::Assistant { items } => Some(items),
            _ => None,
        }
    }

    pub fn display_text(&self) -> Cow<'_, str> {
        match self {
            Self::Compaction { .. } => Cow::Borrowed(""),
            Self::System { text } => Cow::Borrowed(text),
            Self::User { content } => content.display_text(),
            Self::Assistant { items } => render_assistant_items(items),
            Self::ScriptResult { output, .. } => output.display_text(),
        }
    }
}

fn render_assistant_items(items: &[AssistantItem]) -> Cow<'_, str> {
    let mut rendered = Cow::Borrowed("");
    for item in items {
        if matches!(item, AssistantItem::Reasoning { .. })
            || matches!(item, AssistantItem::Text { text, .. } if text.is_empty())
        {
            continue;
        }
        if !rendered.is_empty() {
            rendered.to_mut().push_str("\n\n");
        }
        match item {
            AssistantItem::Reasoning { .. } => {}
            AssistantItem::Text { text, .. } => {
                if rendered.is_empty() {
                    rendered = Cow::Borrowed(text);
                } else {
                    rendered.to_mut().push_str(text);
                }
            }
            AssistantItem::ScriptCall { input, .. } => {
                let rendered = rendered.to_mut();
                rendered.push_str("<tool_call>\n");
                rendered.push_str(input);
                rendered.push_str("\n</tool_call>");
            }
        }
    }
    rendered
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "serialization tests assert roundtrip content"
)]
mod tests {
    use super::*;

    #[test]
    fn ordered_content_roundtrips_without_embedding_image_bytes() -> Result<(), serde_json::Error> {
        let image = ImageAttachment {
            id: "a".repeat(64),
            mime_type: "image/png".into(),
            width: 2,
            height: 3,
            byte_length: 100,
        };
        let content = MessageContent(vec![
            ContentPart::Text {
                text: "before".into(),
            },
            ContentPart::Image {
                image: image.clone(),
            },
            ContentPart::Text {
                text: "after".into(),
            },
        ]);
        let encoded = serde_json::to_string(&content)?;
        assert_eq!(serde_json::from_str::<MessageContent>(&encoded)?, content);
        assert!(content.display_text().starts_with("before\n[Image "));
        assert!(content.display_text().ends_with("\nafter"));
        assert_eq!(content.images().collect::<Vec<_>>(), vec![&image]);
        assert!(!encoded.contains("base64"));
        assert!(!content.without_images().has_images());
        assert!(content.without_images().display_text().contains(&image.id));
        Ok(())
    }
}
