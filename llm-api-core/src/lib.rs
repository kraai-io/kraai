use std::{collections::BTreeMap, sync::Arc};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct LLMManager {
    clients: BTreeMap<ClientId, Arc<dyn LLMClient>>,
    cached_models: BTreeMap<ClientId, Vec<Model>>,
}

impl Default for LLMManager {
    fn default() -> Self {
        Self {
            clients: BTreeMap::new(),
            cached_models: BTreeMap::new(),
        }
    }
}

impl LLMManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn add_client(&mut self, client: impl LLMClient + 'static) -> Result<()> {
        let client_id = client.get_client_id();
        self.clients.insert(client_id.clone(), Arc::new(client));
        self.update_models(client_id).await?;
        Ok(())
    }

    pub fn get_models(&self, client_id: impl Into<ClientId>) -> &Vec<Model> {
        self.cached_models.get(&client_id.into()).unwrap()
    }

    pub async fn update_models(&mut self, client_id: ClientId) -> Result<()> {
        let client = self.clients.get(&client_id).unwrap();
        self.cached_models
            .insert(client_id, client.get_models().await?);
        Ok(())
    }

    pub async fn generate_reply(
        &self,
        client_id: ClientId,
        model_id: ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let client = self.clients.get(&client_id).unwrap();
        client.generate_reply(model_id, messages).await
    }
}

#[async_trait::async_trait]
pub trait LLMClient: Send + Sync {
    fn get_client_id(&self) -> ClientId;

    async fn generate_reply(
        &self,
        model_id: ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage>;

    async fn get_models(&self) -> Result<Vec<Model>>;
}

pub type ClientId = String;

pub type ModelId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: ModelId,
    pub name: String,
    pub max_context: Option<usize>,
    pub supports_streaming: Option<bool>,
}

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChatRole {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "user")]
    User,
    #[serde(rename = "assistant")]
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
        match s.to_lowercase().as_str().trim() {
            "system" => ChatRole::System,
            "user" => ChatRole::User,
            "assistant" => ChatRole::Assistant,
            _ => ChatRole::User, // Default to user for unknown roles
        }
    }
}
