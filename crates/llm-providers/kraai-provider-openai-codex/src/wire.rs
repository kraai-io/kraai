use serde::{Deserialize, Serialize};

use crate::messages::ResponsesRequestMessage;

#[derive(Serialize)]
pub struct ResponsesRequest {
    pub model: String,
    pub instructions: String,
    pub input: Vec<ResponsesRequestMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ResponsesReasoning>,
    pub stream: bool,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResponsesReasoning {
    pub effort: String,
}

#[derive(Deserialize)]
pub struct ListModelsResponse {
    #[serde(default)]
    pub data: Vec<ListModelEntry>,
    #[serde(default)]
    pub models: Vec<ListModelEntry>,
}

#[derive(Deserialize)]
pub struct ListModelEntry {
    #[serde(alias = "slug")]
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, alias = "max_tokens")]
    pub max_context: Option<usize>,
}

impl ListModelsResponse {
    pub fn into_models(self) -> Vec<ListModelEntry> {
        if !self.data.is_empty() {
            self.data
        } else {
            self.models
        }
    }
}

#[derive(Deserialize)]
pub struct ResponsesStreamEvent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub delta: Option<String>,
    #[serde(default)]
    pub response: Option<ResponsesCompletedResponse>,
}

#[derive(Deserialize)]
pub struct ResponsesCompletedResponse {
    #[serde(default)]
    pub usage: Option<ResponsesUsage>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResponsesUsage {
    #[serde(default)]
    pub input_tokens: usize,
    #[serde(default)]
    pub output_tokens: usize,
    #[serde(default)]
    pub input_tokens_details: Option<ResponsesInputTokenDetails>,
    #[serde(default)]
    pub output_tokens_details: Option<ResponsesOutputTokenDetails>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResponsesInputTokenDetails {
    #[serde(default)]
    pub cached_tokens: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResponsesOutputTokenDetails {
    #[serde(default)]
    pub reasoning_tokens: Option<usize>,
}

#[derive(Deserialize)]
pub struct ResponsesOutput {
    #[serde(default)]
    pub output: Vec<ResponseOutputItem>,
}

#[derive(Deserialize)]
pub struct ResponseOutputItem {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub content: Vec<ResponseContentItem>,
}

#[derive(Deserialize)]
pub struct ResponseContentItem {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::ResponsesRequestMessage;
    use serde_json::json;

    #[test]
    fn responses_request_serializes_prompt_cache_key_when_present() {
        let request = ResponsesRequest {
            model: "gpt-5.2-codex".to_string(),
            instructions: "instructions".to_string(),
            input: Vec::<ResponsesRequestMessage>::new(),
            reasoning: None,
            stream: true,
            store: false,
            prompt_cache_key: Some("session-123".to_string()),
        };

        let serialized = serde_json::to_value(request).expect("serialized request");

        assert_eq!(serialized["prompt_cache_key"], json!("session-123"));
    }

    #[test]
    fn responses_request_omits_prompt_cache_key_when_missing() {
        let request = ResponsesRequest {
            model: "gpt-5.2-codex".to_string(),
            instructions: "instructions".to_string(),
            input: Vec::<ResponsesRequestMessage>::new(),
            reasoning: None,
            stream: true,
            store: false,
            prompt_cache_key: None,
        };

        let serialized = serde_json::to_value(request).expect("serialized request");

        assert!(serialized.get("prompt_cache_key").is_none());
    }
}
