use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<RequestMessage>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
}

#[derive(Debug, Serialize)]
pub struct RequestMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionResponse {
    pub choices: Vec<ChatCompletionChoice>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionChoice {
    pub message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
pub struct ResponseMessage {
    pub role: String,
    pub content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionChunk {
    pub choices: Vec<ChatCompletionChunkChoice>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionChunkChoice {
    pub delta: ChatCompletionChunkDelta,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionChunkDelta {
    pub content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListModelsResponse {
    pub data: Vec<ListModelEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ListModelEntry {
    pub id: String,
}
