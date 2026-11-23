use kraai_provider_core::{DynamicConfig, DynamicValue, ModelOptionsProtocol};

use crate::wire::{CacheControl, RequestContent, RequestContentPart, RequestMessage};

/// Anthropic allows four cache breakpoints per request.
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

    /// Explicit settings keep the hints when `base_url` later points elsewhere.
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
mod tests;
