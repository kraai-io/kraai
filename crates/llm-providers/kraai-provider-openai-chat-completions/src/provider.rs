use kraai_io::http::{build_streaming_http_client, finite_request};
use std::collections::BTreeMap;
use std::marker::PhantomData;

use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use kraai_provider_core::{
    ConfiguredModelMetadata, DEFAULT_HTTP_RETRY_POLICY, DynamicConfig, DynamicValue, Model,
    ModelConfig, ModelMetadataCache, Provider, ProviderFactory, ProviderPricingPolicy,
    ProviderRequest, ProviderRequestContext, ProviderStreamEvent, ResolvedImages,
    ScriptToolDefinition, send_with_retry, stream_sse_data,
};
use kraai_types::{ModelId, ProviderId};
use reqwest::{Client, Response};

use crate::auth::ApiKeyAuth;
use crate::messages::normalize_chat_messages;
use crate::models::ModelResolver;
use crate::profile::{
    ChatCompletionsProfile, GenericChatCompletionsProfile, OpenAiChatCompletionsProfile,
};
use crate::streaming::adapt_chat_completion_stream;
use crate::wire::{
    ChatCompletionRequest, ChatCompletionStreamOptions, ListModelEntry, ListModelsResponse,
    RequestMessage,
};

pub struct ChatCompletionsProvider<P> {
    id: ProviderId,
    client: Client,
    base_url: String,
    auth: ApiKeyAuth,
    only_listed_models: bool,
    cached_models: ModelMetadataCache<Vec<ListModelEntry>>,
    model_configs: BTreeMap<ModelId, ConfiguredModelMetadata>,
    model_catalog: Option<std::sync::Arc<kraai_provider_core::ModelCatalog>>,
    catalog_provider: Option<String>,
    _profile: PhantomData<P>,
}

impl<P> ChatCompletionsProvider<P>
where
    P: ChatCompletionsProfile,
{
    fn build_endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), path)
    }

    fn model_resolver(&self) -> ModelResolver<'_> {
        ModelResolver {
            configs: &self.model_configs,
            only_listed: self.only_listed_models,
            provider: self.catalog_provider.as_deref(),
            api: &self.base_url,
            protocol: self.options_protocol(),
        }
    }

    async fn discover_models(&self) -> Result<Vec<ListModelEntry>> {
        let response = send_with_retry(
            "list models",
            &DEFAULT_HTTP_RETRY_POLICY,
            &ProviderRequestContext::default(),
            || {
                finite_request(
                    self.auth
                        .apply(self.client.get(self.build_endpoint("models"))),
                )
                .send()
            },
        )
        .await?;
        let response = ensure_success_response("list models", response).await?;
        let bytes = kraai_io::http::read_response_body(response, 32 * 1024 * 1024).await?;
        let models: ListModelsResponse = serde_json::from_slice(&bytes)?;
        if let Some(catalog) = &self.model_catalog {
            catalog.initialize().await;
        }
        Ok(models.data)
    }

    async fn send_chat_completion_request(
        &self,
        operation: &'static str,
        request: &serde_json::Value,
        headers: &BTreeMap<String, String>,
        request_context: &ProviderRequestContext,
    ) -> Result<Response> {
        let response = send_with_retry(
            operation,
            &DEFAULT_HTTP_RETRY_POLICY,
            request_context,
            || {
                let mut builder = self
                    .auth
                    .apply(self.client.post(self.build_endpoint("chat/completions")))
                    .json(request);
                for (name, value) in headers {
                    builder = builder.header(name, value);
                }
                if request.get("stream").and_then(serde_json::Value::as_bool) == Some(true) {
                    builder.send()
                } else {
                    finite_request(builder).send()
                }
            },
        )
        .await?;

        ensure_success_response(operation, response).await
    }

    fn options_protocol(&self) -> kraai_provider_core::ModelOptionsProtocol {
        match self.catalog_provider.as_deref() {
            Some("openrouter") => {
                return kraai_provider_core::ModelOptionsProtocol::OpenRouterChatCompletions;
            }
            Some("deepseek") => {
                return kraai_provider_core::ModelOptionsProtocol::DeepSeekChatCompletions;
            }
            _ => {}
        }
        match reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|url| url.host_str().map(ToString::to_string))
            .as_deref()
        {
            Some("openrouter.ai") => {
                kraai_provider_core::ModelOptionsProtocol::OpenRouterChatCompletions
            }
            Some("api.deepseek.com") => {
                kraai_provider_core::ModelOptionsProtocol::DeepSeekChatCompletions
            }
            _ => kraai_provider_core::ModelOptionsProtocol::OpenAiChatCompletions,
        }
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
        self.cached_models
            .models(self.model_catalog.as_deref(), |native, catalog| {
                self.model_resolver().resolve(native, catalog)
            })
            .await
            .values()
            .cloned()
            .collect()
    }

    async fn get_model(&self, model_id: &ModelId) -> Option<Model> {
        self.cached_models
            .models(self.model_catalog.as_deref(), |native, catalog| {
                self.model_resolver().resolve(native, catalog)
            })
            .await
            .get(model_id)
            .cloned()
    }

    fn set_model_catalog(&mut self, catalog: std::sync::Arc<kraai_provider_core::ModelCatalog>) {
        self.model_catalog = Some(catalog);
        self.cached_models.invalidate();
    }

    async fn cache_models(&self) -> Result<()> {
        self.cached_models
            .refresh(
                self.model_catalog.as_deref(),
                self.discover_models(),
                |native, catalog| self.model_resolver().resolve(native, catalog),
            )
            .await
    }

    async fn register_model(&mut self, model: ModelConfig) -> Result<()> {
        let metadata = ConfiguredModelMetadata::from_model_config(&model)?;
        self.model_configs.insert(model.id, metadata);
        self.cached_models.invalidate();
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        provider_request: ProviderRequest,
        request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        let model = self.get_model(model_id).await;
        let definitions = model
            .as_ref()
            .map(|model| model.options.as_slice())
            .unwrap_or_default();
        kraai_types::validate_model_options(definitions, &provider_request.options)
            .map_err(|errors| eyre!("Invalid model options: {errors:?}"))?;
        kraai_provider_core::validate_model_option_effects(definitions, &provider_request.options)?;
        let options = provider_request.options;
        let supports_images = self
            .model_configs
            .get(model_id)
            .and_then(|metadata| metadata.supports_images)
            .unwrap_or(model.as_ref().is_some_and(|model| model.supports_images));
        let images = ResolvedImages::for_model(
            &provider_request.messages,
            model_id,
            supports_images,
            request_context,
        )
        .await?;
        let tool_name = provider_request
            .script_tool
            .as_ref()
            .map(|tool| tool.name.clone());
        let messages = normalize_chat_messages(provider_request.messages, &images, &self.id)?;
        let has_tool_history = messages.iter().any(|message| match message {
            RequestMessage::Assistant { tool_calls, .. } => !tool_calls.is_empty(),
            RequestMessage::Tool { .. } => true,
            _ => false,
        });
        let tools = provider_request
            .script_tool
            .or_else(|| has_tool_history.then(ScriptToolDefinition::nushell))
            .map(|tool| vec![tool.into()]);
        let request = ChatCompletionRequest {
            tools,
            tool_choice: if tool_name.is_some() { "auto" } else { "none" },
            parallel_tool_calls: tool_name.as_ref().map(|_| false),
            model: model_id.to_string(),
            messages,
            stream: true,
            stream_options: Some(ChatCompletionStreamOptions {
                include_usage: true,
            }),
        };

        let mut body = serde_json::to_value(request)?;
        let headers = kraai_provider_core::apply_model_options(definitions, &options, &mut body)?;
        let response = self
            .send_chat_completion_request(
                "chat completions stream",
                &body,
                &headers,
                request_context,
            )
            .await?;

        Ok(adapt_chat_completion_stream(
            stream_sse_data(response),
            tool_name,
            reqwest::Url::parse(&self.base_url)
                .ok()
                .is_some_and(|url| url.host_str() == Some("openrouter.ai")),
        ))
    }
}

async fn ensure_success_response(operation: &str, response: Response) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let url = response.url().to_string();
    let body = kraai_provider_core::read_error_body(response).await;

    if let Some(error) = kraai_provider_core::ProviderError::from_api_error(&body) {
        return Err(error.into());
    }
    Err(eyre!(
        "Chat completions {operation} failed with status {status} at {url}: {body}"
    ))
}

fn create_provider<P>(id: ProviderId, config: DynamicConfig) -> Result<ChatCompletionsProvider<P>>
where
    P: ChatCompletionsProfile,
{
    let base_url = P::base_url(&config)?;
    let auth = ApiKeyAuth::resolve(&config)?;
    let only_listed_models = config
        .get("only_listed_models")
        .and_then(DynamicValue::as_bool)
        .unwrap_or(true);

    let catalog_provider = P::pricing_catalog(&config).provider;
    Ok(ChatCompletionsProvider::<P> {
        model_catalog: None,
        catalog_provider,
        id,
        client: build_streaming_http_client()?,
        base_url,
        auth,
        only_listed_models,
        cached_models: ModelMetadataCache::default(),
        model_configs: BTreeMap::new(),
        _profile: PhantomData,
    })
}

pub struct OpenAiChatCompletionsFactory;

impl ProviderFactory for OpenAiChatCompletionsFactory {
    const TYPE_ID: &'static str = GenericChatCompletionsProfile::TYPE_ID;

    fn definition() -> kraai_provider_core::ProviderDefinition {
        GenericChatCompletionsProfile::definition()
    }

    fn pricing_policy() -> ProviderPricingPolicy {
        GenericChatCompletionsProfile::pricing_policy()
    }

    fn create(id: ProviderId, config: DynamicConfig) -> Result<Box<dyn Provider>> {
        Ok(Box::new(create_provider::<GenericChatCompletionsProfile>(
            id, config,
        )?))
    }

    fn validate_provider_config(
        config: &DynamicConfig,
    ) -> Vec<kraai_provider_core::ValidationError> {
        GenericChatCompletionsProfile::validate_provider_config(config)
    }

    fn validate_model_config(config: &DynamicConfig) -> Vec<kraai_provider_core::ValidationError> {
        GenericChatCompletionsProfile::validate_model_config(config)
    }
}

pub struct OpenAiFactory;

impl ProviderFactory for OpenAiFactory {
    const TYPE_ID: &'static str = OpenAiChatCompletionsProfile::TYPE_ID;

    fn definition() -> kraai_provider_core::ProviderDefinition {
        OpenAiChatCompletionsProfile::definition()
    }

    fn pricing_policy() -> ProviderPricingPolicy {
        OpenAiChatCompletionsProfile::pricing_policy()
    }

    fn create(id: ProviderId, config: DynamicConfig) -> Result<Box<dyn Provider>> {
        Ok(Box::new(create_provider::<OpenAiChatCompletionsProfile>(
            id, config,
        )?))
    }

    fn validate_provider_config(
        config: &DynamicConfig,
    ) -> Vec<kraai_provider_core::ValidationError> {
        OpenAiChatCompletionsProfile::validate_provider_config(config)
    }

    fn validate_model_config(config: &DynamicConfig) -> Vec<kraai_provider_core::ValidationError> {
        OpenAiChatCompletionsProfile::validate_model_config(config)
    }
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
