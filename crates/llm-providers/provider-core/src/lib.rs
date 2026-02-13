use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use color_eyre::Result;
use futures::{future::join_all, stream::BoxStream};
use serde::{Deserialize, Serialize};
use types::ChatMessage;

#[derive(Default)]
pub struct ProviderManager {
    pub providers: BTreeMap<ProviderId, Box<dyn Provider>>,
}

#[derive(Default)]
pub struct ProviderManagerHelper {
    factory_registry: BTreeMap<String, ProviderFactoryFn>,
}

type ProviderFactoryFn = Box<dyn Fn(ProviderId, toml::Value) -> Result<Box<dyn Provider>> + Send>;

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

impl ProviderManagerHelper {
    pub fn register_factory<F: ProviderFactory + 'static>(&mut self) {
        let key = F::TYPE.to_string();
        let factory_fn = |id, config: toml::Value| {
            let config: F::Config = config.try_into().unwrap();
            F::create(id, config)
        };
        self.factory_registry.insert(key, Box::new(factory_fn));
    }
}

impl ProviderManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn load_config(
        &mut self,
        config: ProviderManagerConfig,
        helper: ProviderManagerHelper,
    ) -> Result<()> {
        let providers = config
            .providers
            .into_iter()
            .map(|x| {
                Ok((
                    x.id.clone(),
                    (helper
                        .factory_registry
                        .get(&x.r#type)
                        .unwrap_or_else(|| panic!("unknown provider: {:#?}", x.r#type))(
                        x.id, x.config,
                    )?),
                ))
            })
            .collect::<Result<Vec<(ProviderId, Box<dyn Provider>)>>>()?;
        self.providers.clear();
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

    pub async fn list_all_models(&self) -> HashMap<ProviderId, Vec<Model>> {
        join_all(
            self.providers
                .iter()
                .map(|(id, x)| async { (id.clone(), x.list_models().await) })
                .collect::<Vec<_>>(),
        )
        .await
        .into_iter()
        .collect()
    }

    pub async fn update_models_list(&mut self) -> Result<()> {
        for p in self.providers.values_mut() {
            p.cache_models().await?;
        }
        Ok(())
    }

    pub async fn generate_reply(
        &self,
        provider_id: ProviderId,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let client = self.providers.get(&provider_id).unwrap();
        client.generate_reply(model_id, messages).await
    }

    pub async fn generate_reply_stream(
        &self,
        provider_id: ProviderId,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let client = self.providers.get(&provider_id).unwrap();
        client.generate_reply_stream(model_id, messages).await
    }
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    ///Unique identifier for this provider (ex: openai, mistral_local).
    fn get_provider_id(&self) -> ProviderId;

    async fn list_models(&self) -> Vec<Model>;

    async fn cache_models(&self) -> Result<()>;

    async fn register_model(&mut self, model: ModelConfig) -> Result<()>;

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

pub type ProviderId = Arc<String>;

pub type ModelId = Arc<String>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: ModelId,
    pub name: String,
    pub max_context: Option<usize>,
}
