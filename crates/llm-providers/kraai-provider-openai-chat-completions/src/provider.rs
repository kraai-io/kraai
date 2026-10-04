use std::collections::BTreeMap;
use std::marker::PhantomData;

use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use kraai_provider_core::{
    ConfiguredModelMetadata, DEFAULT_HTTP_RETRY_POLICY, DynamicConfig, DynamicValue, Model,
    ModelConfig, Provider, ProviderFactory, ProviderPricingPolicy, ProviderRequest,
    ProviderRequestContext, ProviderStreamEvent, ResolvedImages, ScriptToolDefinition,
    build_streaming_http_client, finite_request, send_with_retry, stream_sse_data,
};
use kraai_types::{ModelId, ProviderId};
use reqwest::{Client, Response};
use tokio::sync::RwLock;

use crate::auth::ApiKeyAuth;
use crate::messages::normalize_chat_messages;
use crate::profile::{
    ChatCompletionsProfile, GenericChatCompletionsProfile, OpenAiChatCompletionsProfile,
};
use crate::streaming::adapt_chat_completion_stream;
use crate::wire::{
    ChatCompletionRequest, ChatCompletionStreamOptions, ListModelsResponse, RequestMessage,
};

pub struct ChatCompletionsProvider<P> {
    id: ProviderId,
    client: Client,
    base_url: String,
    auth: ApiKeyAuth,
    only_listed_models: bool,
    cached_models: RwLock<BTreeMap<ModelId, Model>>,
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

    async fn send_chat_completion_request(
        &self,
        operation: &'static str,
        request: &ChatCompletionRequest,
        request_context: &ProviderRequestContext,
    ) -> Result<Response> {
        let response = send_with_retry(
            operation,
            &DEFAULT_HTTP_RETRY_POLICY,
            request_context,
            || {
                let builder = self
                    .auth
                    .apply(self.client.post(self.build_endpoint("chat/completions")))
                    .json(request);
                if request.stream {
                    builder.send()
                } else {
                    finite_request(builder).send()
                }
            },
        )
        .await?;

        ensure_success_response(operation, response).await
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

    async fn get_model(&self, model_id: &ModelId) -> Option<Model> {
        self.cached_models.read().await.get(model_id).cloned()
    }

    fn set_model_catalog(&mut self, catalog: std::sync::Arc<kraai_provider_core::ModelCatalog>) {
        self.model_catalog = Some(catalog);
    }

    async fn cache_models(&self) -> Result<()> {
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
        let models = response.json::<ListModelsResponse>().await?;

        if let Some(catalog) = &self.model_catalog {
            catalog.initialize().await;
        }
        let mut cache = BTreeMap::new();

        for model in models.data {
            let raw_id = model.id;
            let id = ModelId::new(raw_id.clone());
            let configured = self.model_configs.get(&id);
            if self.only_listed_models && configured.is_none() {
                continue;
            }
            let catalog = match &self.model_catalog {
                Some(catalog) => {
                    catalog
                        .metadata(
                            self.catalog_provider.as_deref(),
                            Some(&self.base_url),
                            &raw_id,
                        )
                        .await
                }
                None => None,
            };

            cache.insert(
                id.clone(),
                configured.cloned().unwrap_or_default().resolve(id, catalog),
            );
        }
        let previous = std::mem::replace(&mut *self.cached_models.write().await, cache);
        drop(previous);

        Ok(())
    }

    async fn register_model(&mut self, model: ModelConfig) -> Result<()> {
        let metadata = ConfiguredModelMetadata::from_config(&model.config)?;
        self.model_configs.insert(model.id, metadata);
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        provider_request: ProviderRequest,
        request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        let supports_images = self
            .model_configs
            .get(model_id)
            .and_then(|metadata| metadata.supports_images)
            .unwrap_or(
                self.get_model(model_id)
                    .await
                    .is_some_and(|model| model.supports_images),
            );
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

        let response = self
            .send_chat_completion_request("chat completions stream", &request, request_context)
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
    let body = response
        .text()
        .await
        .unwrap_or_else(|error| format!("<failed to read body: {error}>"));

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
        cached_models: RwLock::new(BTreeMap::new()),
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
#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "provider tests use direct assertions for local HTTP fixtures"
)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use futures::StreamExt;
    use kraai_provider_core::{ProviderRequestContext, ProviderRetryEvent, ProviderRetryObserver};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn provider_creation_uses_the_same_catalog_identity_as_pricing() {
        let credentials = DynamicConfig::from([("api_key".into(), DynamicValue::from("fixture"))]);
        let direct = create_provider::<OpenAiChatCompletionsProfile>(
            ProviderId::new("direct"),
            credentials.clone(),
        )
        .unwrap();
        assert_eq!(direct.catalog_provider.as_deref(), Some("openai"));
        for (endpoint, override_id, expected) in [
            ("https://api.openai.com/v1/", None, Some("openai")),
            ("https://api.openai.com/v1", Some("custom"), Some("custom")),
            ("https://proxy.test/v1", Some("custom"), Some("custom")),
            ("https://api.groq.com/openai/v1", None, None),
        ] {
            let mut config = credentials.clone();
            config.insert("base_url".into(), DynamicValue::from(endpoint));
            if let Some(id) = override_id {
                config.insert("catalog_provider".into(), DynamicValue::from(id));
            }
            let pricing = GenericChatCompletionsProfile::pricing_catalog(&config);
            let provider = create_provider::<GenericChatCompletionsProfile>(
                ProviderId::new("compatible"),
                config,
            )
            .unwrap();
            assert_eq!(provider.catalog_provider.as_deref(), expected);
            assert_eq!(provider.catalog_provider, pricing.provider);
        }
    }

    fn is_missing_system_ca_error(error: &dyn std::error::Error) -> bool {
        let mut current = Some(error);
        while let Some(error) = current {
            let display = error.to_string();
            let debug = format!("{error:?}");
            if display.contains("No CA certificates were loaded from the system")
                || debug.contains("No CA certificates were loaded from the system")
                || display == "builder error"
            {
                return true;
            }
            current = error.source();
        }
        false
    }

    fn test_client_or_skip() -> Option<Client> {
        match Client::builder().timeout(Duration::from_secs(2)).build() {
            Ok(client) => Some(client),
            Err(error) if is_missing_system_ca_error(&error) => None,
            Err(error) => panic!("unexpected reqwest client build error: {error}"),
        }
    }

    #[derive(Clone, Default)]
    struct RetryCollector {
        events: Arc<Mutex<Vec<ProviderRetryEvent>>>,
    }

    impl RetryCollector {
        fn snapshot(&self) -> Vec<ProviderRetryEvent> {
            self.events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    impl ProviderRetryObserver for RetryCollector {
        fn on_retry_scheduled(&self, event: &ProviderRetryEvent) {
            self.events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(event.clone());
        }
    }

    enum ScriptedResponse {
        Status {
            status_line: &'static str,
            body: &'static str,
        },
    }

    async fn spawn_server(script: Vec<ScriptedResponse>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let script = Arc::new(tokio::sync::Mutex::new(VecDeque::from(script)));

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };

                let next = {
                    let mut guard = script.lock().await;
                    guard.pop_front()
                };
                let Some(next) = next else {
                    break;
                };

                let mut buffer = [0_u8; 4096];
                let _ = stream.read(&mut buffer).await;

                match next {
                    ScriptedResponse::Status { status_line, body } => {
                        write_json_response(&mut stream, status_line, body).await;
                    }
                }
            }
        });

        address
    }

    async fn write_json_response(
        stream: &mut tokio::net::TcpStream,
        status_line: &str,
        body: &str,
    ) {
        let response = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );

        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn generate_reply_stream_forwards_retry_observer_to_http_retry_layer() {
        let address = spawn_server(vec![
            ScriptedResponse::Status {
                status_line: "429 Too Many Requests",
                body: r#"{"error":{"message":"slow down"}}"#,
            },
            ScriptedResponse::Status {
                status_line: "200 OK",
                body: "data: {\"choices\":[{\"delta\":{\"content\":\"ok after retry\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            },
        ])
        .await;

        let Some(client) = test_client_or_skip() else {
            return;
        };

        let provider = ChatCompletionsProvider::<GenericChatCompletionsProfile> {
            id: ProviderId::new("openai-chat-completions"),
            client,
            base_url: format!("http://{address}"),
            auth: ApiKeyAuth::resolve(&BTreeMap::from([(
                String::from("api_key"),
                kraai_provider_core::DynamicValue::from("test-key"),
            )]))
            .unwrap(),
            only_listed_models: false,
            cached_models: RwLock::new(BTreeMap::new()),
            model_configs: BTreeMap::new(),
            model_catalog: None,
            catalog_provider: None,
            _profile: PhantomData,
        };

        let collector = Arc::new(RetryCollector::default());
        let events = provider
            .generate_reply_stream(
                &ModelId::new("gpt-4.1-mini"),
                ProviderRequest {
                    cacheable_messages: None,
                    messages: vec![kraai_types::ConversationItem::User {
                        content: String::from("hello").into(),
                    }],
                    script_tool: None,
                },
                &ProviderRequestContext::with_retry_observer(collector.clone()),
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            events.first(),
            Some(Ok(ProviderStreamEvent::TextDelta { delta, .. }))
                if delta == "ok after retry"
        ));

        let retries = collector.snapshot();
        assert_eq!(retries.len(), 1);
        assert_eq!(retries[0].operation, "chat completions stream");
        assert_eq!(retries[0].retry_number, 1);
        assert_eq!(retries[0].reason, "HTTP 429 Too Many Requests");
    }

    #[tokio::test]
    async fn successful_http_status_does_not_hide_a_streamed_provider_failure() {
        let address = spawn_server(vec![ScriptedResponse::Status {
            status_line: "200 OK",
            body: concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
                "data: {\"error\":{\"code\":502,\"message\":\"Provider disconnected\"},\"choices\":[{\"delta\":{\"content\":\"\"},\"finish_reason\":\"error\"}]}\n\n",
                "data: [DONE]\n\n",
            ),
        }])
        .await;
        let provider = ChatCompletionsProvider::<GenericChatCompletionsProfile> {
            id: ProviderId::new("fixture"),
            client: Client::builder().tls_certs_only([]).build().unwrap(),
            base_url: format!("http://{address}"),
            auth: ApiKeyAuth::resolve(&BTreeMap::from([(
                String::from("api_key"),
                DynamicValue::from("test-key"),
            )]))
            .unwrap(),
            only_listed_models: false,
            cached_models: RwLock::new(BTreeMap::new()),
            model_configs: BTreeMap::new(),
            model_catalog: None,
            catalog_provider: None,
            _profile: PhantomData,
        };
        let events = provider
            .generate_reply_stream(
                &ModelId::new("fixture-model"),
                ProviderRequest {
                    cacheable_messages: None,
                    messages: vec![kraai_types::ConversationItem::User {
                        content: String::from("hello").into(),
                    }],
                    script_tool: None,
                },
                &ProviderRequestContext::default(),
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(matches!(
            events.first(),
            Some(Ok(ProviderStreamEvent::TextDelta { delta, .. })) if delta == "partial"
        ));
        assert!(events.get(1).is_some_and(|event| {
            event
                .as_ref()
                .is_err_and(|error| error.to_string().contains("Provider disconnected"))
        }));
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn model_cache_replacement_preserves_selection_metadata_and_failed_refreshes() {
        for only_listed_models in [false, true] {
            let address = spawn_server(vec![
                ScriptedResponse::Status {
                    status_line: "200 OK",
                    body: r#"{"data":[{"id":"zeta"},{"id":"alpha"},{"id":"alpha"}]}"#,
                },
                ScriptedResponse::Status {
                    status_line: "200 OK",
                    body: r#"{"data":[{"id":42}]}"#,
                },
            ])
            .await;
            let Some(client) = test_client_or_skip() else {
                return;
            };
            let provider = ChatCompletionsProvider::<GenericChatCompletionsProfile> {
                id: ProviderId::new("fixture"),
                client,
                base_url: format!("http://{address}"),
                auth: ApiKeyAuth::resolve(&BTreeMap::from([(
                    String::from("api_key"),
                    DynamicValue::from("fixture-key"),
                )]))
                .unwrap(),
                only_listed_models,
                cached_models: RwLock::new(BTreeMap::from([(
                    ModelId::new("stale"),
                    Model {
                        supports_images: false,
                        id: ModelId::new("stale"),
                        name: String::from("Stale model"),
                        max_context: None,
                    },
                )])),
                model_configs: BTreeMap::from([(
                    ModelId::new("alpha"),
                    ConfiguredModelMetadata {
                        supports_images: None,
                        name: Some(String::from("Configured alpha")),
                        max_context: Some(4096),
                    },
                )]),
                model_catalog: None,
                catalog_provider: None,
                _profile: PhantomData,
            };
            let metadata = |models: Vec<Model>| {
                models
                    .into_iter()
                    .map(|model| (model.id.to_string(), model.name, model.max_context))
                    .collect::<Vec<_>>()
            };
            let mut expected = vec![(
                String::from("alpha"),
                String::from("Configured alpha"),
                Some(4096),
            )];
            if !only_listed_models {
                expected.push((String::from("zeta"), String::from("zeta"), None));
            }

            provider.cache_models().await.unwrap();
            assert_eq!(metadata(provider.list_models().await), expected);
            for listed in provider.list_models().await {
                let found = provider.get_model(&listed.id).await.unwrap();
                assert_eq!(metadata(vec![found]), metadata(vec![listed]));
            }
            assert!(provider.get_model(&ModelId::new("unknown")).await.is_none());
            assert!(provider.get_model(&ModelId::new("stale")).await.is_none());
            if only_listed_models {
                assert!(provider.get_model(&ModelId::new("zeta")).await.is_none());
            }
            assert!(provider.cache_models().await.is_err());
            assert_eq!(metadata(provider.list_models().await), expected);
        }
    }
}
