use anyhow::{Result, anyhow};
use llm_api_core::{ChatMessage, ClientId, LLMClient, Model, ModelId};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

pub const OPEN_WEBUI_CLIENT_ID: &str = "open-webui";

#[derive(Clone)]
pub struct OpenWebuiClient {
    base_url: Url,
    api_key: Option<String>,
    client: Client,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Usage,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stream: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl OpenWebuiClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: Url::parse(base_url).unwrap(),
            api_key: None,
            client: Client::new(),
        }
    }

    pub fn with_api_key(base_url: &str, api_key: impl Into<String>) -> Self {
        Self {
            base_url: Url::parse(base_url).unwrap(),
            api_key: Some(api_key.into()),
            client: Client::new(),
        }
    }

    pub fn set_api_key(&mut self, api_key: impl Into<String>) {
        self.api_key = Some(api_key.into());
    }

    pub fn remove_api_key(&mut self) {
        self.api_key = None;
    }
}

#[async_trait::async_trait]
impl LLMClient for OpenWebuiClient {
    fn get_client_id(&self) -> ClientId {
        OPEN_WEBUI_CLIENT_ID.to_string()
    }

    async fn generate_reply(
        &self,
        model_id: ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let url = self.base_url.join("/api/chat/completions").unwrap();

        let request_json = json!({
            "model": model_id,
            "messages": messages,
            "stream": false,
        });

        let mut request = self.client.post(url);

        if let Some(ref api_key) = self.api_key {
            request = request.bearer_auth(api_key);
        }

        let response = request.json(&request_json).send().await?;

        if !response.status().is_success() {
            return Err(anyhow!("Failed to generate reply: {}", response.status()));
        }

        #[derive(Debug, Deserialize)]
        struct Response {
            choices: Vec<Choice>,
        }

        #[derive(Debug, Deserialize)]
        struct Choice {
            message: ChatMessage,
        }

        Ok(response
            .json::<Response>()
            .await
            .unwrap()
            .choices
            .first()
            .unwrap()
            .message
            .clone())
    }

    async fn get_models(&self) -> Result<Vec<Model>> {
        let url = self.base_url.join("/api/models").unwrap();

        let mut request = self.client.get(url);

        if let Some(ref api_key) = self.api_key {
            request = request.bearer_auth(api_key);
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            return Err(anyhow!("Failed to fetch models: {}", response.status()));
        }

        #[derive(Deserialize)]
        struct Response {
            data: Vec<Model>,
        }

        Ok(response.json::<Response>().await?.data)
    }
}
