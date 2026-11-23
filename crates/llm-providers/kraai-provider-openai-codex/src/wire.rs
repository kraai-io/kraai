use serde::{Deserialize, Serialize};

use crate::messages::ResponsesRequestItem;

#[derive(Serialize)]
pub struct ResponsesRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<&'static str>,
    pub instructions: String,
    pub input: Vec<ResponsesRequestItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ResponsesReasoning>,
    pub tools: Vec<ResponsesCustomTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<usize>,
    pub stream: bool,
    pub store: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<&'static str>,
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
    pub models: Vec<ListModelEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ListModelEntry {
    pub slug: String,
    pub display_name: String,
    pub visibility: ModelVisibility,
    pub context_window: Option<usize>,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub supported_reasoning_levels: Vec<ReasoningLevel>,
    #[serde(default)]
    pub service_tiers: Vec<ModelServiceTier>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModelVisibility {
    List,
    Hide,
    None,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReasoningLevel {
    pub effort: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelServiceTier {
    pub id: String,
    pub name: String,
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

#[derive(Debug, Deserialize, Serialize)]
pub struct ResponseOutputItem {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

#[cfg(test)]
mod tests;
