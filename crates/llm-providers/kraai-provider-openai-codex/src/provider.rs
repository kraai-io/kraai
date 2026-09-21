use std::collections::BTreeMap;
use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use kraai_provider_core::{
    ConfiguredModelMetadata, DEFAULT_HTTP_RETRY_POLICY, DynamicConfig, DynamicValue,
    FieldDefinition, FieldValueKind, Model, ModelConfig, Provider, ProviderDefinition,
    ProviderPricingPolicy, ProviderRequest, ProviderRequestContext, ProviderStreamEvent,
    ScriptToolTransport, ValidationError, finite_request, send_with_retry as send_http_with_retry,
    stream_sse_data, streaming_http_client_builder,
};
use kraai_types::{ModelId, ProviderId};
use reqwest::header::{ACCEPT, HeaderValue};
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use tokio::sync::RwLock;
use tracing::{error, warn};

use crate::auth::{OpenAiCodexAuthController, OpenAiCodexRequestAuth};
use crate::messages::normalize_conversation;
use crate::models::DiscoveredModels;
use crate::streaming::adapt_responses_stream;
use crate::wire::{ListModelsResponse, ResponsesCustomTool, ResponsesRequest};

const DEFAULT_CHATGPT_BACKEND_URL: &str = "https://chatgpt.com/backend-api";
const CODEX_CLIENT_VERSION: &str = "0.154.0";
const BACKEND_URL_ERROR: &str = "OpenAI Codex backend URL must use HTTPS; HTTP is allowed only for loopback endpoints with proxy-token authentication";

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod discovery_tests;

fn valid_backend_url(value: &str, proxy_token: bool) -> bool {
    Url::parse(value).is_ok_and(|url| {
        valid_backend_transport(&url, proxy_token)
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

fn valid_backend_transport(url: &Url, proxy_token: bool) -> bool {
    url.scheme() == "https"
        || (url.scheme() == "http"
            && proxy_token
            && match url.host() {
                Some(url::Host::Ipv4(address)) => address.is_loopback(),
                Some(url::Host::Ipv6(address)) => address.is_loopback(),
                Some(url::Host::Domain(host)) => host == "localhost",
                None => false,
            })
}

fn codex_redirect_policy(proxy_token: bool, origin: url::Origin) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if !valid_backend_transport(attempt.url(), proxy_token) {
            attempt.error(BACKEND_URL_ERROR)
        } else if !proxy_token && attempt.url().origin() != origin {
            attempt.error(
                "OpenAI Codex subscription redirects must stay on the configured backend origin",
            )
        } else if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}

fn build_codex_http_client(proxy_token: bool, base_url: &str) -> Result<Client> {
    Ok(streaming_http_client_builder()
        .https_only(!proxy_token)
        .redirect(codex_redirect_policy(
            proxy_token,
            Url::parse(base_url)?.origin(),
        ))
        .build()?)
}

fn valid_environment_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[derive(Clone)]
enum RequestAuthentication {
    Subscription(OpenAiCodexRequestAuth),
    ProxyToken(String),
}

impl RequestAuthentication {
    fn apply(&self, builder: RequestBuilder) -> RequestBuilder {
        match self {
            Self::Subscription(auth) => auth.apply_chatgpt_headers(builder),
            Self::ProxyToken(token) => builder.bearer_auth(token),
        }
    }
}

pub struct OpenAiCodexFactory {
    auth: Arc<OpenAiCodexAuthController>,
}

impl OpenAiCodexFactory {
    pub const TYPE_ID: &'static str = "openai-codex";

    pub fn new(auth: Arc<OpenAiCodexAuthController>) -> Self {
        Self { auth }
    }

    pub fn pricing_policy() -> ProviderPricingPolicy {
        crate::pricing::pricing_policy()
    }

    pub fn definition() -> ProviderDefinition {
        ProviderDefinition {
            type_id: String::new(),
            display_name: "OpenAI Codex".to_string(),
            protocol_family: "openai-responses".to_string(),
            description: "OpenAI Codex provider using ChatGPT/Codex subscription auth".to_string(),
            provider_fields: vec![
                FieldDefinition {
                    key: "base_url".to_string(),
                    label: "Backend URL".to_string(),
                    value_kind: FieldValueKind::Url,
                    required: false,
                    secret: false,
                    help_text: Some(
                        "ChatGPT backend URL; override only for a trusted proxy".to_string(),
                    ),
                    default_value: Some(DynamicValue::String(
                        DEFAULT_CHATGPT_BACKEND_URL.to_string(),
                    )),
                },
                FieldDefinition {
                    key: "proxy_token_env".to_string(),
                    label: "Proxy Token Env Var".to_string(),
                    value_kind: FieldValueKind::String,
                    required: false,
                    secret: false,
                    help_text: Some(
                        "Short-lived proxy token environment variable; evaluation use only"
                            .to_string(),
                    ),
                    default_value: None,
                },
            ],
            model_fields: ConfiguredModelMetadata::fields(),
            supports_model_discovery: true,
            default_provider_id_prefix: "openai-codex".to_string(),
        }
    }

    pub fn validate_provider_config(config: &DynamicConfig) -> Vec<ValidationError> {
        let proxy_token = config
            .get("proxy_token_env")
            .and_then(DynamicValue::as_str)
            .is_some_and(|name| !name.trim().is_empty());
        let mut errors = match config.get("base_url") {
            None => Vec::new(),
            Some(value)
                if value
                    .as_str()
                    .is_some_and(|url| valid_backend_url(url.trim(), proxy_token)) =>
            {
                Vec::new()
            }
            Some(_) => vec![ValidationError {
                field: "base_url".to_string(),
                message: BACKEND_URL_ERROR.to_string(),
            }],
        };
        if let Some(value) = config.get("proxy_token_env")
            && value
                .as_str()
                .is_none_or(|name| name.trim().is_empty() || !valid_environment_name(name.trim()))
        {
            errors.push(ValidationError {
                field: "proxy_token_env".to_string(),
                message: "Proxy token environment variable name is invalid".to_string(),
            });
        }
        errors
    }

    pub fn validate_model_config(config: &DynamicConfig) -> Vec<ValidationError> {
        ConfiguredModelMetadata::validate(config)
    }

    pub fn create(&self, id: ProviderId, config: DynamicConfig) -> Result<Box<dyn Provider>> {
        let base_url = config
            .get("base_url")
            .and_then(DynamicValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_CHATGPT_BACKEND_URL)
            .trim_end_matches('/')
            .to_string();
        let proxy_token = config
            .get("proxy_token_env")
            .and_then(DynamicValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|name| {
                std::env::var(name).map_err(|error| {
                    eyre!("OpenAI Codex proxy token environment variable is missing: {error}")
                })
            })
            .transpose()?;
        if !valid_backend_url(&base_url, proxy_token.is_some()) {
            return Err(eyre!(BACKEND_URL_ERROR));
        }
        if proxy_token.is_some() && base_url == DEFAULT_CHATGPT_BACKEND_URL {
            return Err(eyre!(
                "OpenAI Codex proxy token requires a non-default backend URL"
            ));
        }
        Ok(Box::new(OpenAiCodexProvider {
            id,
            auth: self.auth.clone(),
            client: build_codex_http_client(proxy_token.is_some(), &base_url)?,
            models: RwLock::new(DiscoveredModels::default()),
            model_configs: BTreeMap::new(),
            base_url,
            proxy_token,
        }))
    }
}

pub struct OpenAiCodexProvider {
    id: ProviderId,
    auth: Arc<OpenAiCodexAuthController>,
    client: Client,
    models: RwLock<DiscoveredModels>,
    model_configs: BTreeMap<ModelId, ConfiguredModelMetadata>,
    base_url: String,
    proxy_token: Option<String>,
}

#[async_trait::async_trait]
impl Provider for OpenAiCodexProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn pricing_model_id(&self, model_id: &ModelId) -> Result<ModelId> {
        Ok(ModelId::new(
            self.models.read().await.resolve(model_id)?.api_model,
        ))
    }

    async fn list_models(&self) -> Vec<Model> {
        self.models.read().await.list(&self.model_configs)
    }

    async fn get_model(&self, model_id: &ModelId) -> Option<Model> {
        self.models.read().await.get(model_id, &self.model_configs)
    }

    async fn cache_models(&self) -> Result<()> {
        let models = self.fetch_models().await?;
        let previous = std::mem::replace(&mut *self.models.write().await, models);
        drop(previous);
        Ok(())
    }

    async fn register_model(&mut self, model: ModelConfig) -> Result<()> {
        let metadata = ConfiguredModelMetadata::from_config(&model.config)?;
        self.model_configs.insert(model.id, metadata);
        Ok(())
    }

    fn script_tool_transport(&self, _model_id: &ModelId) -> ScriptToolTransport {
        ScriptToolTransport::NativeCustom
    }

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        request: ProviderRequest,
        request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        let response = self
            .send_responses_request(model_id, request, request_context)
            .await?;
        Ok(adapt_responses_stream(stream_sse_data(response)))
    }
}

impl OpenAiCodexProvider {
    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn authenticated_get(&self, url: &str, auth: RequestAuthentication) -> RequestBuilder {
        finite_request(
            self.apply_chatgpt_headers(self.client.get(url), auth)
                .header(ACCEPT, HeaderValue::from_static("application/json")),
        )
    }

    fn authenticated_post(&self, url: &str, auth: RequestAuthentication) -> RequestBuilder {
        self.apply_chatgpt_headers(self.client.post(url), auth)
    }

    fn apply_chatgpt_headers(
        &self,
        builder: RequestBuilder,
        auth: RequestAuthentication,
    ) -> RequestBuilder {
        auth.apply(builder)
    }

    async fn fetch_models(&self) -> Result<DiscoveredModels> {
        let mut url = Url::parse(&self.endpoint("codex/models"))?;
        url.query_pairs_mut()
            .append_pair("client_version", CODEX_CLIENT_VERSION);
        let request_context = ProviderRequestContext::default();
        let response = self
            .send_authenticated_request("list models", &request_context, |auth| {
                self.authenticated_get(url.as_str(), auth)
            })
            .await?;
        let response = response.json::<ListModelsResponse>().await?;
        DiscoveredModels::new(response.models)
    }

    async fn send_responses_request(
        &self,
        model_id: &ModelId,
        provider_request: ProviderRequest,
        request_context: &ProviderRequestContext,
    ) -> Result<Response> {
        let tool = provider_request
            .script_tool
            .ok_or_else(|| eyre!("OpenAI Codex request omitted the Kraai script tool"))?;
        let normalized = normalize_conversation(provider_request.messages);
        let resolved_model = self.models.read().await.resolve(model_id)?;
        let request = ResponsesRequest {
            model: resolved_model.api_model,
            instructions: normalized.instructions,
            input: normalized.input,
            reasoning: resolved_model.reasoning,
            tools: [ResponsesCustomTool {
                kind: "custom",
                name: tool.name,
                description: tool.description,
            }],
            tool_choice: Some("auto"),
            parallel_tool_calls: Some(false),
            stream: true,
            store: false,
            prompt_cache_key: request_context.prompt_cache_key().map(ToString::to_string),
        };

        self.send_authenticated_request("responses", request_context, |auth| {
            let builder = self
                .authenticated_post(&self.endpoint("codex/responses"), auth)
                .header(ACCEPT, responses_accept_header())
                .json(&request);
            apply_responses_session_headers(builder, request_context.prompt_cache_key())
        })
        .await
    }

    async fn send_authenticated_request<F>(
        &self,
        operation: &'static str,
        request_context: &ProviderRequestContext,
        build: F,
    ) -> Result<Response>
    where
        F: Fn(RequestAuthentication) -> RequestBuilder + Send + Sync,
    {
        if !valid_backend_url(&self.base_url, self.proxy_token.is_some()) {
            return Err(eyre!(BACKEND_URL_ERROR));
        }
        if let Some(token) = &self.proxy_token {
            let auth = RequestAuthentication::ProxyToken(token.clone());
            let response = send_http_with_retry(
                operation,
                &DEFAULT_HTTP_RETRY_POLICY,
                request_context,
                || build(auth.clone()).send(),
            )
            .await?;
            return ensure_success_response(operation, response).await;
        }
        let auth = self.auth.get_request_auth().await?;
        let response = send_http_with_retry(
            operation,
            &DEFAULT_HTTP_RETRY_POLICY,
            request_context,
            || build(RequestAuthentication::Subscription(auth.clone())).send(),
        )
        .await?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return ensure_success_response(operation, response).await;
        }

        log_retryable_auth_failure(operation, response).await;

        let refreshed = self.auth.refresh_request_auth(&auth).await?;
        let response = send_http_with_retry(
            operation,
            &DEFAULT_HTTP_RETRY_POLICY,
            request_context,
            || build(RequestAuthentication::Subscription(refreshed.clone())).send(),
        )
        .await?;
        ensure_success_response(operation, response).await
    }
}

fn responses_accept_header() -> HeaderValue {
    HeaderValue::from_static("text/event-stream")
}

fn apply_responses_session_headers(
    builder: RequestBuilder,
    prompt_cache_key: Option<&str>,
) -> RequestBuilder {
    if let Some(prompt_cache_key) = prompt_cache_key {
        builder
            .header("session_id", prompt_cache_key)
            .header("x-client-request-id", prompt_cache_key)
    } else {
        builder
    }
}

async fn log_retryable_auth_failure(operation: &str, response: Response) {
    let url = response.url().to_string();
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|error| format!("<failed to read body: {error}>"));
    warn!(
        operation,
        %status,
        url,
        body,
        "OpenAI Codex request returned unauthorized; attempting token refresh"
    );
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
    error!(
        operation,
        %status,
        url,
        body,
        "OpenAI Codex request failed"
    );
    Err(eyre!(
        "OpenAI Codex {operation} failed with status {status} at {url}: {body}"
    ))
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests use direct assertions for model and request fixtures"
)]
mod tests {
    use super::*;
    use crate::auth::OpenAiCodexAuthControllerOptions;
    use ulid::Ulid;

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

    fn auth_controller() -> Option<OpenAiCodexAuthController> {
        match OpenAiCodexAuthController::new_with_options(OpenAiCodexAuthControllerOptions::new(
            std::env::temp_dir()
                .join(format!("provider-openai-codex-{}", Ulid::generate()))
                .join("auth.json"),
        )) {
            Ok(controller) => Some(controller),
            Err(error) if is_missing_system_ca_error(&error) => None,
            Err(error) => panic!("unexpected auth controller init error: {error}"),
        }
    }

    fn test_client_or_skip() -> Option<Client> {
        match Client::builder().build() {
            Ok(client) => Some(client),
            Err(error) if is_missing_system_ca_error(&error) => None,
            Err(error) => panic!("unexpected reqwest client build error: {error}"),
        }
    }

    fn provider() -> Option<OpenAiCodexProvider> {
        let auth = auth_controller()?;
        let client = test_client_or_skip()?;
        Some(OpenAiCodexProvider {
            id: ProviderId::new("openai"),
            auth: Arc::new(auth),
            client,
            models: RwLock::new(DiscoveredModels::default()),
            model_configs: BTreeMap::new(),
            base_url: DEFAULT_CHATGPT_BACKEND_URL.to_string(),
            proxy_token: None,
        })
    }

    #[test]
    fn backend_url_validation_rejects_credentials_queries_and_non_http_schemes() {
        assert!(valid_backend_url("https://chatgpt.com/backend-api", false));
        assert!(valid_backend_url("http://127.0.0.1:1234/backend-api", true));
        assert!(!valid_backend_url("file:///tmp/backend", true));
        assert!(!valid_backend_url(
            "https://user:secret@example.com/backend",
            false
        ));
        assert!(!valid_backend_url(
            "https://example.com/backend?redirect=elsewhere",
            false
        ));
    }

    #[test]
    fn configured_backend_url_controls_codex_endpoints() {
        let Some(mut provider) = provider() else {
            return;
        };
        provider.base_url = String::from("http://127.0.0.1:4321/backend-api");
        assert_eq!(
            provider.endpoint("codex/responses"),
            "http://127.0.0.1:4321/backend-api/codex/responses"
        );
    }

    #[test]
    fn codex_models_use_native_custom_tools() {
        let Some(provider) = provider() else {
            return;
        };

        assert_eq!(
            provider.script_tool_transport(&ModelId::new("gpt-5.6-sol-high")),
            ScriptToolTransport::NativeCustom
        );
        assert_eq!(
            provider.script_tool_transport(&ModelId::new("gpt-6-astra-ultra")),
            ScriptToolTransport::NativeCustom
        );
        assert_eq!(
            provider.script_tool_transport(&ModelId::new("custom-experimental-model")),
            ScriptToolTransport::NativeCustom
        );
    }

    #[test]
    fn proxy_authentication_uses_only_the_short_lived_bearer_token() {
        let Some(client) = test_client_or_skip() else {
            return;
        };
        let request = RequestAuthentication::ProxyToken(String::from("short-lived"))
            .apply(client.get("http://127.0.0.1/backend-api/models"))
            .build()
            .unwrap();
        assert_eq!(
            request.headers().get("authorization").unwrap(),
            "Bearer short-lived"
        );
        assert!(request.headers().get("chatgpt-account-id").is_none());
    }

    #[test]
    fn responses_session_headers_use_prompt_cache_key() {
        let Some(client) = test_client_or_skip() else {
            return;
        };
        let request = apply_responses_session_headers(
            client.post("https://chatgpt.com/backend-api/codex/responses"),
            Some("session-123"),
        )
        .build()
        .expect("request should build");

        assert_eq!(
            request
                .headers()
                .get("session_id")
                .and_then(|value| value.to_str().ok()),
            Some("session-123")
        );
        assert_eq!(
            request
                .headers()
                .get("x-client-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("session-123")
        );
    }

    #[test]
    fn responses_accept_header_is_event_stream() {
        assert_eq!(
            responses_accept_header(),
            HeaderValue::from_static("text/event-stream")
        );
    }
}
