//! OpenAI-compatible provider implementation.
//!
//! This provider supports any OpenAI-compatible API by allowing a custom base URL.

use std::collections::BTreeMap;

use async_openai::Client;
use color_eyre::eyre::{Result, eyre};
use futures::{StreamExt, stream::BoxStream};
use provider_core::{Model, ModelConfig, Provider, ProviderFactory};
use serde::Deserialize;
use tokio::sync::RwLock;
use types::{ChatMessage, ModelId, ProviderId};

/// OpenAI-compatible provider.
pub struct OpenAIProvider {
    id: ProviderId,
    cached_models: RwLock<BTreeMap<ModelId, Model>>,
    model_configs: BTreeMap<ModelId, OpenAIModelConfig>,
    config: OpenAIConfig,
    client: Client<async_openai::config::OpenAIConfig>,
}

#[async_trait::async_trait]
impl Provider for OpenAIProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<Model> {
        self.cached_models.read().await.values().cloned().collect()
    }

    async fn cache_models(&self) -> Result<()> {
        let mut cache = self.cached_models.write().await;
        cache.clear();
        let models_raw: ListOpenAIModelResponse = self.client.models().list_byot().await?;

        for model in models_raw.data {
            let id = ModelId::new(model.id);
            let contains = self.model_configs.contains_key(&id);
            if self.config.only_listed_models && !contains {
                continue;
            }
            let mut name = id.clone().to_string();
            let mut max_context = None;
            if let Some(m) = self.model_configs.get(&id) {
                max_context = m.max_context;
                if let Some(n) = &m.name {
                    name = n.clone();
                }
            }
            cache.insert(
                id.clone(),
                Model {
                    id,
                    name,
                    max_context,
                },
            );
        }
        Ok(())
    }

    async fn register_model(&mut self, model: ModelConfig) -> Result<()> {
        let config: OpenAIModelConfig = model.config.try_into()?;
        self.model_configs.insert(config.id.clone(), config);
        Ok(())
    }

    async fn generate_reply(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let request = serde_json::json!({
            "model": model_id,
            "messages": self.serialize_messages(messages)?
        });
        let response: serde_json::Value = self.client.chat().create_byot(request).await?;
        let message = response["choices"][0]["message"]
            .as_object()
            .ok_or_else(|| eyre!("Invalid response: missing message"))?;
        let content = message
            .get("content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| eyre!("Invalid response: missing content"))?
            .to_string();
        let role = message
            .get("role")
            .ok_or_else(|| eyre!("Invalid response: missing role"))?;
        let role = serde_json::from_value(role.clone())?;
        Ok(ChatMessage { content, role })
    }

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let request = serde_json::json!({
            "model": model_id,
            "messages": self.serialize_messages(messages)?,
            "stream": true
        });
        let stream: BoxStream<Result<String>> = self
            .client
            .chat()
            .create_stream_byot(request)
            .await?
            .filter_map(|x: Result<serde_json::Value, _>| async {
                match x {
                    Ok(val) => {
                        let content = val["choices"][0]["delta"]["content"].as_str();
                        content.map(|s: &str| Ok(s.to_string()))
                    }
                    Err(e) => Some(Err(eyre!(e))),
                }
            })
            .boxed();

        Ok(stream)
    }
}

impl OpenAIProvider {
    fn serialize_messages(&self, messages: Vec<ChatMessage>) -> Result<serde_json::Value> {
        let converted: Vec<serde_json::Value> = messages
            .into_iter()
            .map(|msg| {
                if msg.role == types::ChatRole::Tool {
                    serde_json::json!({
                        "role": "user",
                        "content": format!("[Tool Result]\n{}", msg.content)
                    })
                } else {
                    serde_json::to_value(&msg).unwrap_or_else(|_| {
                        serde_json::json!({
                            "role": serde_json::to_value(&msg.role).unwrap_or(serde_json::json!("user")),
                            "content": msg.content
                        })
                    })
                }
            })
            .collect();
        Ok(serde_json::to_value(converted)?)
    }
}

/// Model configuration for OpenAI provider.
#[derive(Deserialize)]
pub struct OpenAIModelConfig {
    pub id: ModelId,
    pub name: Option<String>,
    pub max_context: Option<usize>,
}

/// Provider configuration for OpenAI-compatible APIs.
#[derive(Deserialize)]
pub struct OpenAIConfig {
    #[allow(rustdoc::bare_urls)]
    /// Base URL for the API (e.g., "https://api.openai.com/v1").
    pub base_url: String,
    /// API key. If not provided, falls back to `env_var_api_key`.
    pub api_key: Option<String>,
    /// Environment variable name for the API key.
    /// Defaults to "OPENAI_API_KEY" if not specified.
    #[serde(default = "default_env_var")]
    pub env_var_api_key: String,
    /// Only list models that are explicitly configured.
    #[serde(default)]
    pub only_listed_models: bool,
}

fn default_env_var() -> String {
    "OPENAI_API_KEY".to_string()
}

/// Factory for creating OpenAI providers.
pub struct OpenAIFactory;

impl ProviderFactory for OpenAIFactory {
    const TYPE: &'static str = "openai";

    type Config = OpenAIConfig;

    fn create(id: ProviderId, config: Self::Config) -> Result<Box<dyn Provider>> {
        let api_key = config
            .api_key
            .clone()
            .or_else(|| std::env::var(&config.env_var_api_key).ok())
            .ok_or_else(|| {
                eyre!(
                    "API key not found. Either set 'api_key' in config or set the '{}' environment variable",
                    config.env_var_api_key
                )
            })?;

        let base_url = config.base_url.clone();
        let cconfig = async_openai::config::OpenAIConfig::new()
            .with_api_base(base_url)
            .with_api_key(api_key);
        let client = Client::with_config(cconfig);
        let provider = OpenAIProvider {
            id,
            cached_models: RwLock::new(BTreeMap::new()),
            model_configs: BTreeMap::new(),
            config,
            client,
        };
        Ok(Box::new(provider))
    }
}

#[derive(Debug, Deserialize)]
struct ListOpenAIModelResponse {
    pub data: Vec<OpenAIModel>,
}

#[derive(Debug, Deserialize)]
struct OpenAIModel {
    pub id: String,
}
