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
