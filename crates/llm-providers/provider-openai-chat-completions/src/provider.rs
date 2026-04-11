use std::collections::BTreeMap;
use std::marker::PhantomData;

use color_eyre::eyre::{Result, eyre};
use futures::{StreamExt, stream::BoxStream};
use provider_core::{
    DEFAULT_HTTP_RETRY_POLICY, DynamicConfig, DynamicValue, Model, ModelConfig, Provider,
    ProviderFactory, ProviderRequestContext, send_with_retry,
};
use reqwest::{Client, Response};
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
                self.auth
                    .apply(self.client.post(self.build_endpoint("chat/completions")))
                    .json(request)
                    .send()
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

    async fn cache_models(&self) -> Result<()> {
        let response = send_with_retry(
            "list models",
            &DEFAULT_HTTP_RETRY_POLICY,
            &ProviderRequestContext::default(),
            || {
                self.auth
                    .apply(self.client.get(self.build_endpoint("models")))
                    .send()
            },
        )
        .await?;
        let response = ensure_success_response("list models", response).await?;
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
        request_context: &ProviderRequestContext,
    ) -> Result<ChatMessage> {
        let request = ChatCompletionRequest {
            model: model_id.to_string(),
            messages: normalize_chat_messages(messages)?,
            stream: false,
        };

        let response = self
            .send_chat_completion_request("chat completions", &request, request_context)
            .await?
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
        request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let request = ChatCompletionRequest {
            model: model_id.to_string(),
            messages: normalize_chat_messages(messages)?,
            stream: true,
        };

        let response = self
            .send_chat_completion_request("chat completions stream", &request, request_context)
            .await?;

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

    Err(eyre!(
        "Chat completions {operation} failed with status {status} at {url}: {body}"
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use provider_core::{ProviderRequestContext, ProviderRetryEvent, ProviderRetryObserver};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

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
    async fn generate_reply_forwards_retry_observer_to_http_retry_layer() {
        let address = spawn_server(vec![
            ScriptedResponse::Status {
                status_line: "429 Too Many Requests",
                body: r#"{"error":{"message":"slow down"}}"#,
            },
            ScriptedResponse::Status {
                status_line: "200 OK",
                body: r#"{"choices":[{"message":{"role":"assistant","content":"ok after retry"}}]}"#,
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
                provider_core::DynamicValue::from("test-key"),
            )]))
            .unwrap(),
            only_listed_models: false,
            cached_models: RwLock::new(BTreeMap::new()),
            model_configs: BTreeMap::new(),
            _profile: PhantomData,
        };

        let collector = Arc::new(RetryCollector::default());
        let reply = provider
            .generate_reply(
                &ModelId::new("gpt-4.1-mini"),
                vec![ChatMessage {
                    role: types::ChatRole::User,
                    content: String::from("hello"),
                }],
                &ProviderRequestContext::with_retry_observer(collector.clone()),
            )
            .await
            .unwrap();

        assert_eq!(reply.content, "ok after retry");

        let events = collector.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].operation, "chat completions");
        assert_eq!(events[0].retry_number, 1);
        assert_eq!(events[0].reason, "HTTP 429 Too Many Requests");
    }
}
