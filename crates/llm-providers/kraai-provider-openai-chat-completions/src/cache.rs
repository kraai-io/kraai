use kraai_provider_core::{DynamicConfig, DynamicValue, ModelOptionsProtocol};

use crate::wire::{CacheControl, RequestContent, RequestContentPart, RequestMessage};

/// Breakpoints after the system prompt, plus this many of the latest messages. Anthropic allows
/// four per request.
const RECENT_BREAKPOINTS: usize = 2;
pub const BREAKPOINTS_KEY: &str = "cache_breakpoints";
pub const PROMPT_CACHE_KEY_KEY: &str = "prompt_cache_key";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheHints {
    pub breakpoints: bool,
    pub prompt_cache_key: bool,
}

impl CacheHints {
    pub fn resolve(config: &DynamicConfig, protocol: ModelOptionsProtocol, base_url: &str) -> Self {
        let flag = |key: &str, default: bool| {
            config
                .get(key)
                .and_then(DynamicValue::as_bool)
                .unwrap_or(default)
        };
        let openai = reqwest::Url::parse(base_url)
            .ok()
            .is_some_and(|url| url.host_str() == Some("api.openai.com"));
        Self {
            breakpoints: flag(
                BREAKPOINTS_KEY,
                protocol == ModelOptionsProtocol::OpenRouterChatCompletions,
            ),
            prompt_cache_key: flag(PROMPT_CACHE_KEY_KEY, openai),
        }
    }

    /// The hints as explicit settings, which keep them when `base_url` later points elsewhere.
    pub fn settings(self) -> DynamicConfig {
        DynamicConfig::from([
            (
                BREAKPOINTS_KEY.to_owned(),
                DynamicValue::Bool(self.breakpoints),
            ),
            (
                PROMPT_CACHE_KEY_KEY.to_owned(),
                DynamicValue::Bool(self.prompt_cache_key),
            ),
        ])
    }
}

/// Marks the system prompt and the latest messages so providers with explicit prompt caching
/// reuse the conversation prefix written by the previous request.
pub fn mark_breakpoints(messages: &mut [RequestMessage]) {
    let system = messages
        .iter()
        .position(|message| matches!(message, RequestMessage::System { .. }))
        .filter(|&index| messages.get(index).is_some_and(has_breakpoint_text));
    let recent: Vec<_> = messages
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, message)| has_breakpoint_text(message))
        .map(|(index, _)| index)
        .take(RECENT_BREAKPOINTS)
        .collect();
    for index in system.into_iter().chain(recent) {
        if let Some(content) = messages.get_mut(index).and_then(breakpoint_content) {
            mark(content);
        }
    }
}

fn breakpoint_content(message: &mut RequestMessage) -> Option<&mut RequestContent> {
    match message {
        RequestMessage::System { content }
        | RequestMessage::User { content }
        | RequestMessage::Tool { content, .. } => Some(content),
        RequestMessage::Assistant { .. } => None,
    }
}

fn has_breakpoint_text(message: &RequestMessage) -> bool {
    match message {
        RequestMessage::System { content }
        | RequestMessage::User { content }
        | RequestMessage::Tool { content, .. } => match content {
            RequestContent::Text(text) => !text.is_empty(),
            RequestContent::Parts(parts) => parts.iter().any(is_text),
        },
        RequestMessage::Assistant { .. } => false,
    }
}

fn is_text(part: &RequestContentPart) -> bool {
    matches!(part, RequestContentPart::Text { text, .. } if !text.is_empty())
}

fn mark(content: &mut RequestContent) {
    if let RequestContent::Text(text) = content {
        *content = RequestContent::Parts(vec![RequestContentPart::Text {
            text: std::mem::take(text),
            cache_control: None,
        }]);
    }
    if let RequestContent::Parts(parts) = content
        && let Some(RequestContentPart::Text { cache_control, .. }) =
            parts.iter_mut().rev().find(|part| is_text(part))
    {
        *cache_control = Some(CacheControl::EPHEMERAL);
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "tests index serialized wire fixtures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(text: &str) -> RequestContent {
        RequestContent::Text(text.into())
    }

    #[test]
    fn marks_the_system_prompt_and_the_two_latest_messages_with_text() {
        let mut messages = vec![
            RequestMessage::System {
                content: text("system"),
            },
            RequestMessage::User {
                content: text("first"),
            },
            RequestMessage::User {
                content: text("second"),
            },
            RequestMessage::Assistant {
                reasoning: Default::default(),
                content: Some("reply".into()),
                tool_calls: Vec::new(),
            },
            RequestMessage::Tool {
                tool_call_id: kraai_types::ToolCallId::new("call"),
                content: text("result"),
            },
            RequestMessage::Tool {
                tool_call_id: kraai_types::ToolCallId::new("empty"),
                content: text(""),
            },
        ];
        mark_breakpoints(&mut messages);
        let marked = |text: &str| json!([{"type": "text", "text": text, "cache_control": {"type": "ephemeral"}}]);
        let body = serde_json::to_value(&messages).unwrap_or_default();
        assert_eq!(body[0]["content"], marked("system"));
        assert_eq!(body[1]["content"], json!("first"));
        assert_eq!(body[2]["content"], marked("second"));
        assert_eq!(body[3]["content"], json!("reply"));
        assert_eq!(body[4]["content"], marked("result"));
        assert_eq!(body[5]["content"], json!(""));
    }

    #[test]
    fn marks_the_last_text_part_of_multipart_content() {
        let mut messages = vec![RequestMessage::User {
            content: RequestContent::Parts(vec![
                RequestContentPart::Text {
                    text: "look".into(),
                    cache_control: None,
                },
                RequestContentPart::ImageUrl {
                    image_url: crate::wire::ImageUrl {
                        url: "data:image/png;base64,AA==".into(),
                    },
                },
            ]),
        }];
        mark_breakpoints(&mut messages);
        let body = serde_json::to_value(&messages).unwrap_or_default();
        assert_eq!(
            body[0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert!(body[0]["content"][1].get("cache_control").is_none());
    }

    #[test]
    fn defaults_follow_the_endpoint_and_config_overrides_them() {
        let empty = DynamicConfig::new();
        assert_eq!(
            CacheHints::resolve(
                &empty,
                ModelOptionsProtocol::OpenRouterChatCompletions,
                "https://openrouter.ai/api/v1"
            ),
            CacheHints {
                breakpoints: true,
                prompt_cache_key: false
            }
        );
        assert_eq!(
            CacheHints::resolve(
                &empty,
                ModelOptionsProtocol::OpenAiChatCompletions,
                "https://api.openai.com/v1"
            ),
            CacheHints {
                breakpoints: false,
                prompt_cache_key: true
            }
        );
        let enabled = DynamicConfig::from([
            ("cache_breakpoints".into(), DynamicValue::Bool(true)),
            ("prompt_cache_key".into(), DynamicValue::Bool(true)),
        ]);
        assert_eq!(
            CacheHints::resolve(
                &enabled,
                ModelOptionsProtocol::OpenAiChatCompletions,
                "http://localhost:8080/v1"
            ),
            CacheHints {
                breakpoints: true,
                prompt_cache_key: true
            }
        );
        assert_eq!(
            CacheHints::resolve(
                &empty,
                ModelOptionsProtocol::OpenAiChatCompletions,
                "http://localhost:8080/v1"
            ),
            CacheHints::default()
        );
    }
}
