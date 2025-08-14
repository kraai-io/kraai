use anyhow::{Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Open WebUI API client for interacting with LLMs
pub struct OpenWebUIClient {
    base_url: String,
    api_key: Option<String>,
    client: Client,
}

/// Chat message structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl std::fmt::Display for ChatMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Role: {}\nMessage:\n{}", self.role, self.content)
    }
}

/// Chat role enumeration for type safety
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

impl std::fmt::Display for ChatRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatRole::System => write!(f, "system"),
            ChatRole::User => write!(f, "user"),
            ChatRole::Assistant => write!(f, "assistant"),
        }
    }
}

impl From<&str> for ChatRole {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "system" => ChatRole::System,
            "user" => ChatRole::User,
            "assistant" => ChatRole::Assistant,
            _ => ChatRole::User, // Default to user for unknown roles
        }
    }
}

/// Chat completion request
#[derive(Debug, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stream: Option<bool>,
}

/// Chat completion response
#[derive(Debug, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Usage,
}

/// Choice in chat completion response
#[derive(Debug, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

/// Token usage information
#[derive(Debug, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Model information
#[derive(Debug, Deserialize)]
pub struct Model {
    pub id: String,
    pub owned_by: String,
    pub name: String,
}

/// Models list response
#[derive(Debug, Deserialize)]
pub struct ModelsResponse {
    pub data: Vec<Model>,
}

impl OpenWebUIClient {
    /// Create a new Open WebUI client
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: None,
            client: Client::new(),
        }
    }

    /// Create a new Open WebUI client with API key
    pub fn with_api_key(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: Some(api_key.into()),
            client: Client::new(),
        }
    }

    /// Set API key for authentication
    pub fn set_api_key(&mut self, api_key: impl Into<String>) {
        self.api_key = Some(api_key.into());
    }

    /// Remove API key
    pub fn remove_api_key(&mut self) {
        self.api_key = None;
    }

    /// Get available models
    pub async fn list_models(&self) -> Result<Vec<Model>> {
        let url = format!("{}/models", self.base_url);
        let mut request = self.client.get(&url);

        if let Some(ref api_key) = self.api_key {
            request = request.header("Authorization", format!("Bearer {}", api_key));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            return Err(anyhow!("Failed to fetch models: {}", response.status()));
        }

        let models_response: ModelsResponse = response.json().await?;
        Ok(models_response.data)
    }

    /// Send a chat completion request
    pub async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        let url = format!("{}/chat/completions", self.base_url);
        let mut request_builder = self.client.post(&url);

        if let Some(ref api_key) = self.api_key {
            request_builder =
                request_builder.header("Authorization", format!("Bearer {}", api_key));
        }

        let response = request_builder.json(&request).send().await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Chat completion failed: {} - {}",
                status,
                error_text
            ));
        }

        let completion_response: ChatCompletionResponse = response.json().await?;
        Ok(completion_response)
    }

    /// Simple chat completion with just model and messages
    pub async fn simple_chat(&self, model: &str, messages: Vec<ChatMessage>) -> Result<String> {
        let request = ChatCompletionRequest {
            model: model.to_string(),
            messages,
            temperature: Some(0.7),
            max_tokens: Some(1000),
            stream: Some(false),
        };

        let response = self.chat_completion(request).await?;

        if let Some(choice) = response.choices.first() {
            Ok(choice.message.content.clone())
        } else {
            Err(anyhow!("No response content received"))
        }
    }

    /// Create a conversation with system message and user input
    pub async fn chat_with_system(
        &self,
        model: &str,
        system_message: &str,
        user_input: &str,
    ) -> Result<String> {
        let messages = vec![
            ChatMessage {
                role: ChatRole::System,
                content: system_message.to_string(),
            },
            ChatMessage {
                role: ChatRole::User,
                content: user_input.to_string(),
            },
        ];

        self.simple_chat(model, messages).await
    }

    /// Get the base URL
    pub fn get_base_url(&self) -> &str {
        &self.base_url
    }

    /// Check if API key is set
    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = OpenWebUIClient::new("http://localhost:3000");
        assert_eq!(client.get_base_url(), "http://localhost:3000");
        assert!(!client.has_api_key());
    }

    #[test]
    fn test_client_with_api_key() {
        let client = OpenWebUIClient::with_api_key("http://localhost:3000", "test-key");
        assert_eq!(client.get_base_url(), "http://localhost:3000");
        assert!(client.has_api_key());
    }

    #[test]
    fn test_chat_message_creation() {
        let message = ChatMessage {
            role: ChatRole::User,
            content: "Hello".to_string(),
        };
        assert_eq!(message.role, ChatRole::User);
        assert_eq!(message.content, "Hello");
    }
}
