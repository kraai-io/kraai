use std::{collections::BTreeMap, sync::Arc};

use color_eyre::Result;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

#[derive(Default)]
pub struct ProviderManager {
    pub providers: BTreeMap<ProviderId, Box<dyn Provider>>,
    factory_registry: BTreeMap<String, ProviderFactoryFn>,
}

type ProviderFactoryFn = Box<dyn Fn(ProviderId, toml::Value) -> Result<Box<dyn Provider>>>;

#[derive(Deserialize, Serialize)]
pub struct ProviderManagerConfig {
    #[serde(default, rename = "provider")]
    providers: Vec<ProviderConfig>,
    #[serde(default, rename = "model")]
    models: Vec<ModelConfig>,
}

#[derive(Deserialize, Serialize)]
pub struct ModelConfig {
    provider_id: ProviderId,
    #[serde(flatten)]
    pub config: toml::Value,
}

#[derive(Deserialize, Serialize)]
pub struct ProviderConfig {
    pub id: ProviderId,
    r#type: String,
    #[serde(flatten)]
    pub config: toml::Value,
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
                    (self
                        .factory_registry
                        .get(&x.r#type)
                        .unwrap_or_else(|| panic!("unknown provider: {:#?}", x.r#type))(
                        x.id, x.config,
                    )?),
                ))
            })
            .collect::<Result<Vec<(ProviderId, Box<dyn Provider>)>>>()?;
        self.providers.extend(providers);

        for m in config.models {
            self.providers
                .get_mut(&m.provider_id)
                .unwrap()
                .register_model(m)
                .await?;
        }

        self.update_models_list().await?;

        Ok(())
    }

    pub fn list_all_models(&self) -> Vec<Model> {
        self.providers
            .values()
            .flat_map(|x| x.list_models())
            .collect()
    }

    pub async fn update_models_list(&mut self) -> Result<()> {
        for p in self.providers.values_mut() {
            p.cache_models().await?;
        }
        Ok(())
    }

    pub async fn generate_reply(
        &mut self,
        client_id: ProviderId,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let client = self.providers.get_mut(&client_id).unwrap();
        client.generate_reply(model_id, messages).await
    }

    pub async fn generate_reply_stream<'a>(
        &'a mut self,
        client_id: ProviderId,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<BoxStream<'a, Result<String>>> {
        let client = self.providers.get_mut(&client_id).unwrap();
        client.generate_reply_stream(model_id, messages).await
    }
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    ///Unique identifier for this provider (ex: openai, mistral_local).
    fn get_provider_id(&self) -> ProviderId;

    fn list_models(&self) -> Vec<Model>;

    async fn cache_models(&mut self) -> Result<()>;

    async fn register_model(&mut self, model: ModelConfig) -> Result<()>;

    async fn generate_reply(
        &mut self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage>;

    async fn generate_reply_stream(
        &mut self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<BoxStream<'static, Result<String>>>;
}

pub type ProviderId = Arc<String>;

pub type ModelId = Arc<String>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: ModelId,
    pub name: String,
    pub max_context: Option<usize>,
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
