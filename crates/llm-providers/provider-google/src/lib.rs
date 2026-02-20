//! Google Gemini provider implementation.
//!
//! This provider uses Google's OpenAI-compatible API endpoint.

use std::collections::BTreeMap;

use async_openai::{Client, config::OpenAIConfig};
use color_eyre::eyre::{Result, eyre};
use futures::{StreamExt, stream::BoxStream};
use provider_core::{Model, ModelConfig, Provider, ProviderFactory};
use serde::Deserialize;
use tokio::sync::RwLock;
use types::{ChatMessage, ModelId, ProviderId};

const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
const DEFAULT_ENV_VAR: &str = "GEMINI_API_KEY";

/// Google Gemini provider.
pub struct GoogleProvider {
    id: ProviderId,
    cached_models: RwLock<BTreeMap<ModelId, Model>>,
    model_configs: BTreeMap<ModelId, GoogleModelConfig>,
    config: GoogleConfig,
    client: Client<OpenAIConfig>,
}

#[async_trait::async_trait]
impl Provider for GoogleProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<Model> {
        self.cached_models.read().await.values().cloned().collect()
    }

    async fn cache_models(&self) -> Result<()> {
        let mut cache = self.cached_models.write().await;
        cache.clear();
        let models_raw: ListGeminiModelResponse = self.client.models().list_byot().await?;

        for model in models_raw.data {
            let id = ModelId::new(model.id.replace("models/", ""));
            let contains = self.model_configs.contains_key(&id);
            if self.config.only_listed_models && !contains {
                continue;
            }
            let mut name = model.display_name;
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
        let config: GoogleModelConfig = model.config.try_into()?;
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

impl GoogleProvider {
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

/// Model configuration for Google provider.
#[derive(Deserialize)]
pub struct GoogleModelConfig {
    pub id: ModelId,
    pub name: Option<String>,
    pub max_context: Option<usize>,
}

/// Provider configuration for Google Gemini.
#[derive(Deserialize)]
pub struct GoogleConfig {
    /// API key. If not provided, falls back to `env_var_api_key`.
    pub api_key: Option<String>,
    /// Environment variable name for the API key.
    /// Defaults to "GEMINI_API_KEY" if not specified.
    #[serde(default = "default_env_var")]
    pub env_var_api_key: String,
    /// Only list models that are explicitly configured.
    #[serde(default)]
    pub only_listed_models: bool,
}

fn default_env_var() -> String {
    DEFAULT_ENV_VAR.to_string()
}

/// Factory for creating Google providers.
pub struct GoogleFactory;

impl ProviderFactory for GoogleFactory {
    const TYPE: &'static str = "google";

    type Config = GoogleConfig;

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

        let cconfig = OpenAIConfig::new()
            .with_api_base(GEMINI_BASE_URL)
            .with_api_key(api_key);
        let client = Client::with_config(cconfig);
        let provider = GoogleProvider {
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
struct ListGeminiModelResponse {
    pub data: Vec<GeminiModel>,
}

#[derive(Debug, Deserialize)]
struct GeminiModel {
    pub id: String,
    pub display_name: String,
}
