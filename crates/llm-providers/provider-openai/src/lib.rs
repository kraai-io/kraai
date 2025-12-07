use std::{collections::BTreeMap, sync::Arc};

use async_openai::Client;
use color_eyre::eyre::{Result, eyre};
use futures::{StreamExt, stream::BoxStream};
use provider_core::{Model, ModelConfig, ModelId, Provider, ProviderFactory, ProviderId};
use serde::Deserialize;
use types::ChatMessage;

pub struct OpenAIProvider {
    id: ProviderId,
    cached_models: BTreeMap<ModelId, Model>,
    model_configs: BTreeMap<ModelId, OpenAIModelConfig>,
    config: OpenAIConfig,
    client: Client<async_openai::config::OpenAIConfig>,
}

impl OpenAIProvider {}

#[async_trait::async_trait]
impl Provider for OpenAIProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    fn list_models(&self) -> Vec<Model> {
        self.cached_models.values().cloned().collect()
    }

    async fn cache_models(&mut self) -> Result<()> {
        self.cached_models.clear();
        let models_raw: ListOpenAIModelResponse = self.client.models().list_byot().await?;

        for model in models_raw.data {
            let id = Arc::new(model.id);
            let contains = self.model_configs.contains_key(&id);
            if self.config.only_listed_models && !contains {
                continue;
            }
            let mut name = model.name;
            let mut max_context = None;
            if contains {
                let m = self.model_configs.get(&id).unwrap();
                max_context = m.max_context;
                if m.name.is_some() {
                    name = m.name.as_ref().unwrap().clone();
                }
            }
            self.cached_models.insert(
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
        &mut self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        if !self.cached_models.contains_key(model_id) {
            self.cache_models().await?;
            if !self.cached_models.contains_key(model_id) {
                return Err(eyre!("unable to find model {}", model_id));
            }
        }

        let request = serde_json::json!({
            "model": model_id,
            "messages": serde_json::to_value(messages)?
        });
        let response: serde_json::Value = self.client.chat().create_byot(request).await?;
        let message = response["choices"][0]["message"].as_object().unwrap();
        let content = message
            .get("content")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let role = serde_json::from_value(message.get("role").unwrap().clone())?;
        let message = ChatMessage { content, role };

        Ok(message)
    }

    async fn generate_reply_stream(
        &mut self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<BoxStream<'static, Result<String>>> {
        if !self.cached_models.contains_key(model_id) {
            self.cache_models().await?;
            if !self.cached_models.contains_key(model_id) {
                return Err(eyre!("unable to find model {}", model_id));
            }
        }

        let request = serde_json::json!({
            "model": model_id,
            "messages": serde_json::to_value(messages)?,
            "stream": true
        });
        let stream: BoxStream<Result<String>> = self
            .client
            .chat()
            .create_stream_byot(request)
            .await?
            .map(|x| {
                x.map_err(|e| eyre!(e)).map(|x: serde_json::Value| {
                    x["choices"][0]["delta"]["content"]
                        .as_str()
                        .unwrap()
                        .to_string()
                })
            })
            .boxed();

        Ok(stream)
    }
}

#[derive(Deserialize)]
pub struct OpenAIModelConfig {
    pub id: ModelId,
    pub name: Option<String>,
    pub max_context: Option<usize>,
}

#[derive(Deserialize)]
pub struct OpenAIConfig {
    pub base_url: String,
    pub env_var_api_key: String,

    #[serde(default)]
    pub only_listed_models: bool,
}

pub struct OpenAIFactory {}

impl ProviderFactory for OpenAIFactory {
    const TYPE: &'static str = "openai";

    type Config = OpenAIConfig;

    fn create(id: ProviderId, config: Self::Config) -> Result<Box<dyn Provider>> {
        let base_url = config.base_url.clone();
        let api_key = std::env::var(config.env_var_api_key.clone())
            .expect(&format!("{} must be set", &config.env_var_api_key));
        let cconfig = async_openai::config::OpenAIConfig::new()
            .with_api_base(base_url)
            .with_api_key(api_key);
        let client = Client::with_config(cconfig);
        let provider = OpenAIProvider {
            id,
            cached_models: BTreeMap::new(),
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
    pub name: String,
}
