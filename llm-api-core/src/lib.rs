use std::{collections::BTreeMap, sync::Arc};

use color_eyre::Result;
use futures::stream::{self, BoxStream, StreamExt};
use serde::{Deserialize, Serialize};

pub struct ProviderManager {
    providers: BTreeMap<ProviderId, Arc<dyn Provider>>,
    models: BTreeMap<ModelId, Model>,
    factory_registry: BTreeMap<String, ProviderFactoryFn>,
}

impl Default for ProviderManager {
    fn default() -> Self {
        Self {
            providers: BTreeMap::new(),
            models: BTreeMap::new(),
            factory_registry: BTreeMap::new(),
        }
    }
}

type ProviderFactoryFn = Box<dyn Fn(ProviderId, toml::Value) -> Result<Box<dyn Provider>>>;

#[derive(Deserialize, Serialize)]
pub struct ProviderManagerConfig {
    providers: Vec<ProviderConfig>,
    models: Vec<ModelConfig>,
}

#[derive(Deserialize, Serialize)]
pub struct ModelConfig {
    id: ModelId,
    provider_id: ProviderId,
    max_context: Option<usize>,
    #[serde(flatten)]
    config: toml::Value,
}

#[derive(Deserialize, Serialize)]
pub struct ProviderConfig {
    id: ProviderId,
    r#type: String,
    #[serde(flatten)]
    config: toml::Value,
}

pub trait ProviderFactory {
    const TYPE: &'static str;

    type Config: for<'de> Deserialize<'de>;

    fn create(id: ProviderId, config: Self::Config) -> Result<Box<dyn Provider>>;
}

impl ProviderManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_factory<F: ProviderFactory + 'static>(&mut self) {
        let key = F::TYPE.to_string();
        let factory_fn = |id, config: toml::Value| {
            let config: F::Config = config.try_into().unwrap();
            F::create(id, config)
        };
        self.factory_registry.insert(key, Box::new(factory_fn));
    }

    pub async fn load_config(&mut self, config: ProviderManagerConfig) -> Result<()> {
        let providers = config
            .providers
            .into_iter()
            .map(|x| {
                Ok((
                    x.id.clone(),
                    Arc::from(self
                        .factory_registry
                        .get(&x.r#type)
                        .expect(&format!("unknown provider: {:#?}", x.r#type))(
                        x.id, x.config,
                    )?),
                ))
            })
            .collect::<Result<Vec<(ProviderId, Arc<dyn Provider>)>>>()?;
        self.providers.extend(providers);

        self.update_models_list().await;

        Ok(())
    }

    pub fn add_provider(&mut self, client: impl Provider + 'static) {
        let provider_id = client.get_provider_id();
        self.providers.insert(provider_id, Arc::new(client));
    }

    pub fn list_all_models(&self) -> Vec<&Model> {
        self.models.values().collect()
    }

    pub async fn update_models_list(&mut self) {
        self.models = stream::iter(self.providers.values())
            .map(|x| x.list_models())
            .buffer_unordered(self.providers.len())
            .flat_map(stream::iter)
            .flat_map(stream::iter)
            .map(|x| (x.id.clone(), x))
            .collect::<BTreeMap<ModelId, Model>>()
            .await;
    }

    pub async fn generate_reply(
        &self,
        client_id: ProviderId,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let client = self.providers.get(&client_id).unwrap();
        client.generate_reply(model_id, messages).await
    }

    pub async fn generate_reply_stream<'a>(
        &'a self,
        client_id: ProviderId,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<BoxStream<'a, Result<String>>> {
        let client = self.providers.get(&client_id).unwrap();
        client.generate_reply_stream(model_id, messages).await
    }
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Unique identifier for this provider (ex: openai, mistral_local).
    fn get_provider_id(&self) -> ProviderId;

    async fn list_models(&self) -> Result<Vec<Model>>;

    async fn register_model(&mut self, model: ModelConfig);

    async fn generate_reply(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage>;

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<BoxStream<'static, Result<String>>>;
}

pub type ProviderId = String;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChatRole {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "user")]
    User,
    #[serde(rename = "assistant")]
    Assistant,
    #[serde(rename = "tool")]
    Tool,
}
