use std::collections::BTreeMap;
use std::marker::PhantomData;

use color_eyre::eyre::{Result, eyre};
use futures::{StreamExt, stream::BoxStream};
use provider_core::{DynamicConfig, DynamicValue, Model, ModelConfig, Provider, ProviderFactory};
use reqwest::Client;
use tokio::sync::RwLock;
use types::{ChatMessage, ModelId, ProviderId};

use crate::auth::ApiKeyAuth;
use crate::messages::{normalize_chat_messages, role_from_wire};
use crate::profile::{
    ChatCompletionsProfile, GenericChatCompletionsProfile, OpenAiChatCompletionsProfile,
};
use crate::sse::stream_sse_data;
use crate::wire::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, ListModelsResponse,
};

#[derive(Clone)]
struct ModelMetadata {
    name: Option<String>,
    max_context: Option<usize>,
}

pub struct ChatCompletionsProvider<P> {
    id: ProviderId,
    client: Client,
    base_url: String,
    auth: ApiKeyAuth,
    only_listed_models: bool,
    cached_models: RwLock<BTreeMap<ModelId, Model>>,
    model_configs: BTreeMap<ModelId, ModelMetadata>,
    _profile: PhantomData<P>,
}

impl<P> ChatCompletionsProvider<P>
where
    P: ChatCompletionsProfile,
{
    fn build_endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), path)
    }
}

#[async_trait::async_trait]
impl<P> Provider for ChatCompletionsProvider<P>
where
    P: ChatCompletionsProfile,
{
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<Model> {
        self.cached_models.read().await.values().cloned().collect()
    }

    async fn cache_models(&self) -> Result<()> {
        let request = self
            .auth
            .apply(self.client.get(self.build_endpoint("models")));
        let response = request.send().await?.error_for_status()?;
        let models = response.json::<ListModelsResponse>().await?;

        let mut cache = self.cached_models.write().await;
        cache.clear();

        for model in models.data {
            let raw_id = model.id;
            let id = ModelId::new(raw_id.clone());
            let configured = self.model_configs.get(&id);
            if self.only_listed_models && configured.is_none() {
                continue;
            }

            cache.insert(
                id.clone(),
                Model {
                    id,
                    name: configured
                        .and_then(|entry| entry.name.clone())
                        .unwrap_or(raw_id),
                    max_context: configured.and_then(|entry| entry.max_context),
                },
            );
        }

        Ok(())
    }

    async fn register_model(&mut self, model: ModelConfig) -> Result<()> {
        let name = model
            .config
            .get("name")
            .and_then(DynamicValue::as_str)
            .map(ToString::to_string)
            .filter(|value| !value.trim().is_empty());
        let max_context = model
            .config
            .get("max_context")
            .and_then(DynamicValue::as_integer)
            .map(usize::try_from)
            .transpose()
            .map_err(|_| eyre!("Invalid max_context"))?;

        self.model_configs
            .insert(model.id, ModelMetadata { name, max_context });
        Ok(())
    }

    async fn generate_reply(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let request = ChatCompletionRequest {
            model: model_id.to_string(),
            messages: normalize_chat_messages(messages)?,
            stream: false,
        };

        let response = self
            .auth
            .apply(self.client.post(self.build_endpoint("chat/completions")))
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json::<ChatCompletionResponse>()
            .await?;

        let message = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| eyre!("Invalid response: missing choices"))?
            .message;

        Ok(ChatMessage {
            role: role_from_wire(&message.role),
            content: message.content.unwrap_or_default(),
        })
    }

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let request = ChatCompletionRequest {
            model: model_id.to_string(),
            messages: normalize_chat_messages(messages)?,
            stream: true,
        };

        let response = self
            .auth
            .apply(self.client.post(self.build_endpoint("chat/completions")))
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let stream = stream_sse_data(response)
            .filter_map(|event| async move {
                match event {
                    Ok(payload) => match serde_json::from_str::<ChatCompletionChunk>(&payload) {
                        Ok(chunk) => chunk
                            .choices
                            .into_iter()
                            .find_map(|choice| choice.delta.content)
                            .map(Ok),
                        Err(error) => Some(Err(eyre!(error))),
                    },
                    Err(error) => Some(Err(error)),
                }
            })
            .boxed();

        Ok(stream)
    }
}

fn create_provider<P>(id: ProviderId, config: DynamicConfig) -> Result<Box<dyn Provider>>
where
    P: ChatCompletionsProfile,
{
    let base_url = P::base_url(&config)?;
    let auth = ApiKeyAuth::resolve(&config)?;
    let only_listed_models = config
        .get("only_listed_models")
        .and_then(DynamicValue::as_bool)
        .unwrap_or(true);

    Ok(Box::new(ChatCompletionsProvider::<P> {
        id,
        client: Client::new(),
        base_url,
        auth,
        only_listed_models,
        cached_models: RwLock::new(BTreeMap::new()),
        model_configs: BTreeMap::new(),
        _profile: PhantomData,
    }))
}

pub struct OpenAiChatCompletionsFactory;

impl ProviderFactory for OpenAiChatCompletionsFactory {
    const TYPE_ID: &'static str = GenericChatCompletionsProfile::TYPE_ID;

    fn definition() -> provider_core::ProviderDefinition {
        GenericChatCompletionsProfile::definition()
    }

    fn create(id: ProviderId, config: DynamicConfig) -> Result<Box<dyn Provider>> {
        create_provider::<GenericChatCompletionsProfile>(id, config)
    }

    fn validate_provider_config(config: &DynamicConfig) -> Vec<provider_core::ValidationError> {
        GenericChatCompletionsProfile::validate_provider_config(config)
    }

    fn validate_model_config(config: &DynamicConfig) -> Vec<provider_core::ValidationError> {
        GenericChatCompletionsProfile::validate_model_config(config)
    }
}

pub struct OpenAiFactory;

impl ProviderFactory for OpenAiFactory {
    const TYPE_ID: &'static str = OpenAiChatCompletionsProfile::TYPE_ID;

    fn definition() -> provider_core::ProviderDefinition {
        OpenAiChatCompletionsProfile::definition()
    }

    fn create(id: ProviderId, config: DynamicConfig) -> Result<Box<dyn Provider>> {
        create_provider::<OpenAiChatCompletionsProfile>(id, config)
    }

    fn validate_provider_config(config: &DynamicConfig) -> Vec<provider_core::ValidationError> {
        OpenAiChatCompletionsProfile::validate_provider_config(config)
    }

    fn validate_model_config(config: &DynamicConfig) -> Vec<provider_core::ValidationError> {
        OpenAiChatCompletionsProfile::validate_model_config(config)
    }
}
