use std::collections::BTreeMap;

use async_openai::{Client, config::OpenAIConfig};
use color_eyre::eyre::{Result, eyre};
use futures::{StreamExt, stream::BoxStream};
use provider_core::{
    ChatMessage, Model, ModelConfig, ModelId, Provider, ProviderFactory, ProviderId,
};
use serde::Deserialize;

pub struct GoogleProvider {
    id: ProviderId,
    cached_models: BTreeMap<ModelId, Model>,
    model_configs: BTreeMap<ModelId, GoogleModelConfig>,
    config: GoogleConfig,
    client: Client<OpenAIConfig>,
}

impl GoogleProvider {}

#[async_trait::async_trait]
impl Provider for GoogleProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    fn list_models(&self) -> Vec<Model> {
        self.cached_models.values().cloned().collect()
    }

    async fn cache_models(&mut self) -> Result<()> {
        self.cached_models.clear();
        let models_raw: ListGeminiModelResponse = self.client.models().list_byot().await?;

        for model in models_raw.data {
            let id = model.id.replace("models/", "");
            let contains = self.model_configs.contains_key(&id);
            if self.config.only_listed_models && !contains {
                continue;
            }
            let mut name = model.display_name;
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
        let config: GoogleModelConfig = model.config.try_into()?;
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
                x.map_err(|e| eyre!(e)).map(|x: serde_json::Value| x["choices"][0]["delta"]["content"]
                        .as_str()
                        .unwrap()
                        .to_string())
            })
            .boxed();

        Ok(stream)
    }
}

#[derive(Deserialize)]
pub struct GoogleModelConfig {
    pub id: ModelId,
    pub name: Option<String>,
    pub max_context: Option<usize>,
}

#[derive(Deserialize)]
pub struct GoogleConfig {
    #[serde(default)]
    pub only_listed_models: bool,
}

pub struct GoogleFactory {}

impl ProviderFactory for GoogleFactory {
    const TYPE: &'static str = "google";

    type Config = GoogleConfig;

    fn create(id: ProviderId, config: Self::Config) -> Result<Box<dyn Provider>> {
        let base_url = "https://generativelanguage.googleapis.com/v1beta/openai";
        let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");
        let cconfig = OpenAIConfig::new()
            .with_api_base(base_url)
            .with_api_key(api_key);
        let client = Client::with_config(cconfig);
        let provider = GoogleProvider {
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
struct ListGeminiModelResponse {
    pub data: Vec<GeminiModel>,
}

#[derive(Debug, Deserialize)]
struct GeminiModel {
    pub id: String,
    pub display_name: String,
}
