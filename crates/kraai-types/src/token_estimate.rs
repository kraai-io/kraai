//! Rough token counts for deciding when a request may outgrow a model's context window.
//! They err high, so a decision based on them happens early rather than late.

use crate::{AssistantItem, ContentPart, ConversationItem, MessageContent, ScriptCall};

/// Bytes of text per estimated token.
pub const BYTES_PER_TOKEN: usize = 4;
const IMAGE_TOKENS: usize = 1_600;
const ITEM_OVERHEAD_TOKENS: usize = 4;

#[must_use]
pub fn estimate_text_tokens(text: &str) -> usize {
    text.len().div_ceil(BYTES_PER_TOKEN)
}

fn estimate_json_tokens(value: &serde_json::Value) -> usize {
    serde_json::to_string(value).map_or(0, |text| estimate_text_tokens(&text))
}

impl MessageContent {
    #[must_use]
    pub fn estimated_tokens(&self) -> usize {
        self.0
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => estimate_text_tokens(text),
                ContentPart::Image { .. } => IMAGE_TOKENS,
            })
            .sum()
    }
}

impl AssistantItem {
    #[must_use]
    pub fn estimated_tokens(&self) -> usize {
        match self {
            Self::Reasoning { payload, .. } => estimate_json_tokens(payload),
            Self::Text { text, .. } => estimate_text_tokens(text),
            Self::ScriptCall(ScriptCall { name, input, .. }) => {
                estimate_text_tokens(name) + estimate_text_tokens(input)
            }
        }
    }
}

impl ConversationItem {
    #[must_use]
    pub fn estimated_tokens(&self) -> usize {
        ITEM_OVERHEAD_TOKENS
            + match self {
                Self::FileContext { text } | Self::System { text } => estimate_text_tokens(text),
                Self::Compaction { payload, .. } => estimate_json_tokens(payload),
                Self::User { content }
                | Self::ScriptResult {
                    output: content, ..
                } => content.estimated_tokens(),
                Self::Assistant { items } => {
                    items.iter().map(AssistantItem::estimated_tokens).sum()
                }
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_round_up_and_count_every_part() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("abcde"), 2);
        let user = ConversationItem::User {
            content: MessageContent(vec![
                ContentPart::Text {
                    text: "x".repeat(400),
                },
                ContentPart::Image {
                    image: kraai_execution_types::ImageAttachment {
                        id: "a".repeat(64),
                        mime_type: "image/png".into(),
                        width: 2,
                        height: 3,
                        byte_length: 100,
                    },
                },
            ]),
        };
        assert_eq!(
            user.estimated_tokens(),
            ITEM_OVERHEAD_TOKENS + 100 + IMAGE_TOKENS
        );
    }
}
