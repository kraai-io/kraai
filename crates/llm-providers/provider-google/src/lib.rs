use std::{collections::BTreeMap, path::PathBuf};

use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use provider_core::{
    ChatMessage, ChatRole, Model, ModelConfig, ModelId, Provider, ProviderFactory, ProviderId,
};
use serde::Deserialize;

const PROVIDER_ID: &str = "google";

pub struct GoogleProvider {
    id: ProviderId,
    models: BTreeMap<ModelId, GoogleModel>,
    config: GoogleConfig,
}

pub struct GoogleModel {
    model: Model,
    config: GoogleModelConfig,
}

#[async_trait::async_trait]
impl Provider for GoogleProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Result<Vec<Model>> {
        Ok(self.models.values().map(|x| x.model.clone()).collect())
    }

    async fn register_model(&mut self, model: ModelConfig) -> Result<()> {
        let config = model.config.try_into()?;
        let model = Model {
            id: model.id.clone(),
            name: model.id,
            max_context: model.max_context,
        };
        self.models
            .insert(model.id.clone(), GoogleModel { model, config });
        Ok(())
    }

    async fn generate_reply(
        &mut self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let model = self.models.get_mut(model_id).unwrap();
        todo!()
    }

    async fn generate_reply_stream(
        &mut self,
        _model_id: &ModelId,
        _messages: Vec<ChatMessage>,
    ) -> Result<BoxStream<'static, Result<String>>> {
        todo!()
    }
}

#[derive(Deserialize)]
pub struct GoogleModelConfig {
    path: String,
}

#[derive(Deserialize)]
pub struct GoogleConfig {}

pub struct GoogleFactory {}

impl ProviderFactory for GoogleFactory {
    const TYPE: &'static str = PROVIDER_ID;

    type Config = GoogleConfig;

    fn create(id: ProviderId, config: Self::Config) -> Result<Box<dyn Provider>> {
        let provider = GoogleProvider {
            id,
            models: BTreeMap::new(),
            config,
        };
        Ok(Box::new(provider))
    }
}
