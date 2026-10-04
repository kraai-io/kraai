use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<RequestMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<FunctionTool>>,
    pub tool_choice: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<ChatCompletionStreamOptions>,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionStreamOptions {
    pub include_usage: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum RequestMessage {
    System {
        content: String,
    },
    User {
        content: RequestContent,
    },
    Assistant {
        #[serde(flatten)]
        reasoning: crate::reasoning::Reasoning,
        content: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<FunctionCall>,
    },
    Tool {
        tool_call_id: kraai_types::ToolCallId,
        content: String,
    },
}

#[derive(Debug, Serialize)]
pub struct FunctionTool {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: FunctionDefinition,
}

#[derive(Debug, Serialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl From<kraai_provider_core::ScriptToolDefinition> for FunctionTool {
    fn from(tool: kraai_provider_core::ScriptToolDefinition) -> Self {
        Self {
            kind: "function",
            function: FunctionDefinition {
                name: tool.name,
                description: tool.description,
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "input": { "type": "string", "description": "Complete Nushell script, including its metadata comment." } },
                    "required": ["input"],
                    "additionalProperties": false,
                }),
            },
        }
    }
}

#[derive(Debug, Serialize)]
pub struct FunctionCall {
    pub id: kraai_types::ToolCallId,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: FunctionArguments,
}

#[derive(Debug, Serialize)]
pub struct FunctionArguments {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptArguments {
    pub input: String,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum RequestContent {
    Text(String),
    Parts(Vec<RequestContentPart>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RequestContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Serialize)]
pub struct ImageUrl {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionChunk {
    #[serde(default)]
    pub error: Option<ChatCompletionError>,
    #[serde(default)]
    pub choices: Vec<ChatCompletionChunkChoice>,
    #[serde(default)]
    pub usage: Option<ChatCompletionUsage>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionError {
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionChunkChoice {
    #[serde(default)]
    pub index: usize,
    pub delta: ChatCompletionChunkDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionChunkDelta {
    #[serde(flatten)]
    pub reasoning: crate::reasoning::Reasoning,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCallDelta>>,
    pub refusal: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ToolCallDelta {
    #[serde(default)]
    pub index: usize,
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub function: Option<FunctionDelta>,
}

#[derive(Debug, Deserialize)]
pub struct FunctionDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChatCompletionUsage {
    #[serde(default)]
    pub cost: Option<serde_json::Value>,
    #[serde(default)]
    pub cost_details: Option<serde_json::Value>,
    #[serde(default)]
    pub prompt_tokens: usize,
    #[serde(default)]
    pub completion_tokens: usize,
    #[serde(default)]
    pub total_tokens: Option<usize>,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokenDetails>,
    #[serde(default)]
    pub completion_tokens_details: Option<CompletionTokenDetails>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PromptTokenDetails {
    #[serde(default)]
    pub cached_tokens: Option<usize>,
    #[serde(default)]
    pub cache_write_tokens: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CompletionTokenDetails {
    #[serde(default)]
    pub reasoning_tokens: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ListModelsResponse {
    pub data: Vec<ListModelEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ListModelEntry {
    pub id: String,
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "wire tests assert after fallible serialization"
)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn disabled_tools_can_retain_a_declaration_for_tool_history() -> color_eyre::Result<()> {
        let tool = || FunctionTool::from(kraai_provider_core::ScriptToolDefinition::nushell());
        for (tools, expected) in [(None, None), (Some(vec![tool()]), Some(json!([tool()])))] {
            let request = ChatCompletionRequest {
                model: "fixture".into(),
                messages: Vec::new(),
                tools,
                tool_choice: "none",
                parallel_tool_calls: None,
                stream: true,
                stream_options: None,
            };
            let encoded = serde_json::to_value(request)?;
            assert_eq!(encoded.get("tools"), expected.as_ref());
            assert_eq!(encoded.get("tool_choice"), Some(&json!("none")));
        }
        Ok(())
    }
}
