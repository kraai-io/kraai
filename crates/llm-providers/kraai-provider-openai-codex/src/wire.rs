use serde::{Deserialize, Serialize};

use crate::messages::ResponsesRequestItem;

#[derive(Serialize)]
pub struct ResponsesRequest {
    pub model: String,
    pub instructions: String,
    pub input: Vec<ResponsesRequestItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ResponsesReasoning>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ResponsesCustomTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    pub stream: bool,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResponsesReasoning {
    pub effort: String,
    pub context: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResponsesCustomTool {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub name: String,
    pub description: String,
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
    pub item_id: Option<String>,
    #[serde(default)]
    pub item: Option<ResponseOutputItem>,
    #[serde(default)]
    pub response: Option<ResponsesCompletedResponse>,
}

#[derive(Deserialize)]
pub struct ResponsesCompletedResponse {
    #[serde(default)]
    pub usage: Option<ResponsesUsage>,
    #[serde(default)]
    pub error: Option<ResponsesError>,
    #[serde(default)]
    pub incomplete_details: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub struct ResponsesError {
    pub code: Option<String>,
    pub message: String,
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

#[derive(Debug, Deserialize)]
pub struct ResponseOutputItem {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub input: Option<String>,
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "tests use direct assertions for serialized wire payloads"
)]
mod tests {
    use super::*;
    use crate::messages::ResponsesRequestItem;
    use serde_json::json;

    #[test]
    fn responses_request_serializes_prompt_cache_key_when_present() {
        let request = ResponsesRequest {
            model: "gpt-5.2-codex".to_string(),
            instructions: "instructions".to_string(),
            input: Vec::<ResponsesRequestItem>::new(),
            reasoning: None,
            tools: Vec::new(),
            tool_choice: None,
            parallel_tool_calls: None,
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
            input: Vec::<ResponsesRequestItem>::new(),
            reasoning: None,
            tools: Vec::new(),
            tool_choice: None,
            parallel_tool_calls: None,
            stream: true,
            store: false,
            prompt_cache_key: None,
        };

        let serialized = serde_json::to_value(request).expect("serialized request");

        assert!(serialized.get("prompt_cache_key").is_none());
        assert!(serialized.get("tools").is_none());
        assert!(serialized.get("tool_choice").is_none());
        assert!(serialized.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn responses_request_registers_exactly_one_raw_custom_tool() {
        let request = ResponsesRequest {
            model: "gpt-5.6-sol".to_string(),
            instructions: "instructions".to_string(),
            input: Vec::<ResponsesRequestItem>::new(),
            reasoning: Some(ResponsesReasoning {
                effort: "high".to_string(),
                context: "current_turn",
            }),
            tools: vec![ResponsesCustomTool {
                kind: "custom",
                name: "kraai_nushell".to_string(),
                description: "Execute Nushell".to_string(),
            }],
            tool_choice: Some("auto"),
            parallel_tool_calls: Some(false),
            stream: true,
            store: false,
            prompt_cache_key: None,
        };

        let serialized = serde_json::to_value(request).expect("serialized request");

        assert_eq!(serialized["tools"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            serialized["tools"][0],
            json!({
                "type": "custom",
                "name": "kraai_nushell",
                "description": "Execute Nushell"
            })
        );
        assert_eq!(serialized["tool_choice"], json!("auto"));
        assert_eq!(serialized["parallel_tool_calls"], json!(false));
        assert_eq!(serialized["reasoning"]["context"], json!("current_turn"));
    }
}
