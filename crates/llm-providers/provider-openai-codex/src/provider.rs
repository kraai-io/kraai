use std::collections::BTreeMap;
use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use futures::{StreamExt, stream::BoxStream};
use provider_core::{
    DynamicConfig, DynamicValue, FieldDefinition, FieldValueKind, Model, ModelConfig, Provider,
    ProviderDefinition, ValidationError,
};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use tokio::sync::RwLock;
use tracing::{error, warn};
use types::{ChatMessage, ChatMessage as ProviderChatMessage, ModelId, ProviderId};

use crate::auth::{OpenAiCodexAuthController, RequestAuth};
use crate::catalog::{CatalogModel, all_catalog_models, title_case_effort, visible_catalog_models};
use crate::messages::normalize_chat_messages;
use crate::wire::{
    ListModelEntry, ListModelsResponse, ResponsesOutput, ResponsesReasoning, ResponsesRequest,
    ResponsesStreamEvent,
};

const CHATGPT_ORIGIN: &str = "https://chatgpt.com";
const CHATGPT_MODELS_ENDPOINT: &str =
    "https://chatgpt.com/backend-api/models?history_and_training_disabled=false";
const CHATGPT_RESPONSES_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_ORIGINATOR: &str = "codex_cli_rs";

#[derive(Clone)]
struct ModelMetadata {
    name: Option<String>,
    max_context: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RemoteModelMetadata {
    title: Option<String>,
    max_context: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedCatalogModel<'a> {
    catalog_model: &'a CatalogModel,
    reasoning_effort: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedRequestModel {
    api_model: String,
    reasoning: Option<ResponsesReasoning>,
}

pub struct OpenAiCodexFactory {
    auth: Arc<OpenAiCodexAuthController>,
}

impl OpenAiCodexFactory {
    pub const TYPE_ID: &'static str = "openai-codex";

    pub fn new(auth: Arc<OpenAiCodexAuthController>) -> Self {
        Self { auth }
    }

    pub fn definition() -> ProviderDefinition {
        ProviderDefinition {
            type_id: String::new(),
            display_name: "OpenAI Codex".to_string(),
            protocol_family: "openai-responses".to_string(),
            description: "OpenAI Codex provider using ChatGPT/Codex subscription auth".to_string(),
            provider_fields: vec![],
            model_fields: vec![
                FieldDefinition {
                    key: "name".to_string(),
                    label: "Display Name".to_string(),
                    value_kind: FieldValueKind::String,
                    required: false,
                    secret: false,
                    help_text: Some("Optional UI name for the model".to_string()),
                    default_value: None,
                },
                FieldDefinition {
                    key: "max_context".to_string(),
                    label: "Max Context".to_string(),
                    value_kind: FieldValueKind::Integer,
                    required: false,
                    secret: false,
                    help_text: Some("Optional context limit in tokens".to_string()),
                    default_value: None,
                },
            ],
            supports_model_discovery: true,
            default_provider_id_prefix: "openai-codex".to_string(),
        }
    }

    pub fn validate_provider_config(_config: &DynamicConfig) -> Vec<ValidationError> {
        Vec::new()
    }

    pub fn validate_model_config(config: &DynamicConfig) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        if let Some(value) = config.get("name")
            && value.as_str().is_none()
        {
            errors.push(ValidationError {
                field: "name".to_string(),
                message: "Display Name must be a string".to_string(),
            });
        }
        if let Some(value) = config.get("max_context") {
            match value.as_integer() {
                Some(number) if number > 0 => {}
                Some(_) => errors.push(ValidationError {
                    field: "max_context".to_string(),
                    message: "Max Context must be greater than zero".to_string(),
                }),
                None => errors.push(ValidationError {
                    field: "max_context".to_string(),
                    message: "Max Context must be an integer".to_string(),
                }),
            }
        }
        errors
    }

    pub fn create(&self, id: ProviderId, _config: DynamicConfig) -> Result<Box<dyn Provider>> {
        Ok(Box::new(OpenAiCodexProvider {
            id,
            auth: self.auth.clone(),
            client: Client::new(),
            cached_models: RwLock::new(BTreeMap::new()),
            model_configs: BTreeMap::new(),
        }))
    }
}

pub struct OpenAiCodexProvider {
    id: ProviderId,
    auth: Arc<OpenAiCodexAuthController>,
    client: Client,
    cached_models: RwLock<BTreeMap<ModelId, Model>>,
    model_configs: BTreeMap<ModelId, ModelMetadata>,
}

#[async_trait::async_trait]
impl Provider for OpenAiCodexProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<Model> {
        self.cached_models.read().await.values().cloned().collect()
    }

    async fn cache_models(&self) -> Result<()> {
        let remote_availability = match self.fetch_remote_model_availability().await {
            Ok(availability) => Some(availability),
            Err(error) => {
                warn!(
                    error = %error,
                    "OpenAI Codex model discovery failed; using bundled model catalog"
                );
                None
            }
        };

        let models = self.discovered_models(remote_availability.as_ref());
        let mut cache = self.cached_models.write().await;
        cache.clear();
        for model in models {
            cache.insert(model.id.clone(), model);
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
        messages: Vec<ProviderChatMessage>,
    ) -> Result<ProviderChatMessage> {
        let response = self
            .send_responses_request(model_id, messages, false)
            .await?
            .json::<ResponsesOutput>()
            .await?;
        let content = extract_response_text(response);

        Ok(ChatMessage {
            role: types::ChatRole::Assistant,
            content,
        })
    }

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        messages: Vec<ProviderChatMessage>,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let response = self
            .send_responses_request(model_id, messages, true)
            .await?;
        let stream = crate::sse::stream_sse_data(response)
            .filter_map(|event| async move {
                match event {
                    Ok(payload) => {
                        let event = match serde_json::from_str::<ResponsesStreamEvent>(&payload) {
                            Ok(event) => event,
                            Err(error) => return Some(Err(eyre!(error))),
                        };

                        match event.kind.as_str() {
                            "response.output_text.delta" => event.delta.map(Ok),
                            "response.completed" => None,
                            "response.failed" | "response.incomplete" => {
                                Some(Err(eyre!("OpenAI response stream failed")))
                            }
                            _ => None,
                        }
                    }
                    Err(error) => Some(Err(error)),
                }
            })
            .boxed();

        Ok(stream)
    }
}

impl OpenAiCodexProvider {
    fn authenticated_get(&self, url: &str, auth: RequestAuth) -> RequestBuilder {
        self.apply_chatgpt_headers(self.client.get(url), auth)
    }

    fn authenticated_post(&self, url: &str, auth: RequestAuth) -> RequestBuilder {
        self.apply_chatgpt_headers(self.client.post(url), auth)
    }

    fn apply_chatgpt_headers(&self, builder: RequestBuilder, auth: RequestAuth) -> RequestBuilder {
        builder
            .bearer_auth(auth.access_token)
            .header("ChatGPT-Account-Id", auth.account_id)
            .header("Accept", "application/json")
            .header("Origin", CHATGPT_ORIGIN)
            .header("Referer", format!("{CHATGPT_ORIGIN}/"))
            .header("User-Agent", CODEX_ORIGINATOR)
            .header("OpenAI-Client-Originator", CODEX_ORIGINATOR)
    }

    async fn fetch_remote_model_availability(
        &self,
    ) -> Result<BTreeMap<String, RemoteModelMetadata>> {
        let response = self
            .send_with_retry("list models", |auth| {
                self.authenticated_get(CHATGPT_MODELS_ENDPOINT, auth)
            })
            .await?;
        let models = response.json::<ListModelsResponse>().await?;

        Ok(models
            .into_models()
            .into_iter()
            .map(
                |ListModelEntry {
                     id,
                     title,
                     max_context,
                 }| { (id, RemoteModelMetadata { title, max_context }) },
            )
            .collect())
    }

    fn discovered_models(
        &self,
        remote_availability: Option<&BTreeMap<String, RemoteModelMetadata>>,
    ) -> Vec<Model> {
        let models = visible_catalog_models()
            .filter_map(|catalog_model| match remote_availability {
                Some(availability) => availability
                    .get(catalog_model.slug)
                    .cloned()
                    .map(|remote| (catalog_model, remote)),
                None => Some((catalog_model, RemoteModelMetadata::default())),
            })
            .flat_map(|(catalog_model, remote)| self.expand_catalog_model(catalog_model, &remote))
            .collect::<Vec<_>>();

        if !models.is_empty() || remote_availability.is_none() {
            return models;
        }

        warn!(
            "OpenAI Codex remote model discovery returned no matching bundled models; falling back to bundled catalog"
        );

        visible_catalog_models()
            .flat_map(|catalog_model| {
                self.expand_catalog_model(catalog_model, &RemoteModelMetadata::default())
            })
            .collect()
    }

    fn expand_catalog_model(
        &self,
        catalog_model: &CatalogModel,
        remote: &RemoteModelMetadata,
    ) -> Vec<Model> {
        catalog_model
            .supported_reasoning_efforts
            .iter()
            .map(|effort| {
                let variant_id = ModelId::new(format!("{}-{}", catalog_model.slug, effort.effort));
                let variant_config = self.model_configs.get(&variant_id);
                let base_config = self
                    .model_configs
                    .get(&ModelId::new(catalog_model.slug.to_string()));
                let max_context = variant_config
                    .and_then(|entry| entry.max_context)
                    .or(base_config.and_then(|entry| entry.max_context))
                    .or(catalog_model.max_context)
                    .or(remote.max_context);

                Model {
                    id: variant_id,
                    name: variant_config
                        .and_then(|entry| entry.name.clone())
                        .unwrap_or_else(|| {
                            format!(
                                "{} ({})",
                                display_name(catalog_model, remote.title.as_deref()),
                                title_case_effort(effort.effort)
                            )
                        }),
                    max_context,
                }
            })
            .collect()
    }

    async fn send_responses_request(
        &self,
        model_id: &ModelId,
        messages: Vec<ProviderChatMessage>,
        stream: bool,
    ) -> Result<Response> {
        let normalized = normalize_chat_messages(messages)?;
        let resolved_model = resolve_request_model(model_id)?;
        let request = ResponsesRequest {
            model: resolved_model.api_model,
            instructions: normalized.instructions,
            input: normalized.input,
            reasoning: resolved_model.reasoning,
            stream,
            store: false,
        };

        self.send_with_retry("responses", |auth| {
            self.authenticated_post(CHATGPT_RESPONSES_ENDPOINT, auth)
                .json(&request)
        })
        .await
    }

    async fn send_with_retry<F>(&self, operation: &'static str, build: F) -> Result<Response>
    where
        F: Fn(RequestAuth) -> RequestBuilder,
    {
        let auth = self.auth.get_request_auth().await?;
        let response = build(auth.clone()).send().await?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return ensure_success_response(operation, response).await;
        }

        log_retryable_auth_failure(operation, response).await;

        let refreshed = self
            .auth
            .refresh_request_auth(Some(auth.account_id.clone()))
            .await?;
        let response = build(refreshed).send().await?;
        ensure_success_response(operation, response).await
    }
}

fn resolve_request_model(model_id: &ModelId) -> Result<ResolvedRequestModel> {
    let raw_model = model_id.to_string();
    let Some(resolved) = resolve_catalog_model(&raw_model)? else {
        return Ok(ResolvedRequestModel {
            api_model: raw_model,
            reasoning: None,
        });
    };

    Ok(ResolvedRequestModel {
        api_model: resolved.catalog_model.slug.to_string(),
        reasoning: Some(ResponsesReasoning {
            effort: resolved.reasoning_effort.to_string(),
        }),
    })
}

fn resolve_catalog_model(raw_model: &str) -> Result<Option<ResolvedCatalogModel<'static>>> {
    let matched = all_catalog_models()
        .iter()
        .filter(|model| {
            raw_model == model.slug || raw_model.starts_with(&format!("{}-", model.slug))
        })
        .max_by_key(|model| model.slug.len());

    let Some(catalog_model) = matched else {
        return Ok(None);
    };

    if raw_model == catalog_model.slug {
        return Ok(Some(ResolvedCatalogModel {
            catalog_model,
            reasoning_effort: catalog_model.default_reasoning_effort.to_string(),
        }));
    }

    let suffix = raw_model
        .strip_prefix(catalog_model.slug)
        .and_then(|value| value.strip_prefix('-'))
        .ok_or_else(|| eyre!("Invalid OpenAI Codex model id '{raw_model}'"))?;

    if catalog_model
        .supported_reasoning_efforts
        .iter()
        .any(|effort| effort.effort == suffix)
    {
        return Ok(Some(ResolvedCatalogModel {
            catalog_model,
            reasoning_effort: suffix.to_string(),
        }));
    }

    Err(eyre!(
        "OpenAI Codex model '{raw_model}' uses unsupported reasoning effort '{suffix}' for base model '{}'",
        catalog_model.slug
    ))
}

fn display_name(catalog_model: &CatalogModel, remote_title: Option<&str>) -> String {
    if !catalog_model.display_name.trim().is_empty() {
        return catalog_model.display_name.to_string();
    }

    remote_title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(catalog_model.slug)
        .to_string()
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

fn extract_response_text(output: ResponsesOutput) -> String {
    output
        .output
        .into_iter()
        .filter(|item| item.kind == "message")
        .flat_map(|item| item.content.into_iter())
        .filter(|item| item.kind == "output_text")
        .filter_map(|item| item.text)
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::OpenAiCodexAuthControllerOptions;
    use provider_core::DynamicConfig;
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
                .join(format!("provider-openai-codex-{}", Ulid::new()))
                .join("auth.json"),
        )) {
            Ok(controller) => Some(controller),
            Err(error) if is_missing_system_ca_error(&error) => None,
            Err(error) => panic!("unexpected auth controller init error: {error}"),
        }
    }

    fn provider() -> Option<OpenAiCodexProvider> {
        let auth = auth_controller()?;
        let client = match Client::builder().build() {
            Ok(client) => client,
            Err(error) if is_missing_system_ca_error(&error) => return None,
            Err(error) => panic!("unexpected reqwest client build error: {error}"),
        };
        Some(OpenAiCodexProvider {
            id: ProviderId::new("openai"),
            auth: Arc::new(auth),
            client,
            cached_models: RwLock::new(BTreeMap::new()),
            model_configs: BTreeMap::new(),
        })
    }

    #[test]
    fn definition_matches_expected_identity() {
        let definition = OpenAiCodexFactory::definition();
        assert_eq!(OpenAiCodexFactory::TYPE_ID, "openai-codex");
        assert_eq!(definition.display_name, "OpenAI Codex");
        assert_eq!(definition.protocol_family, "openai-responses");
        assert!(definition.provider_fields.is_empty());
        assert_eq!(definition.default_provider_id_prefix, "openai-codex");
    }

    #[test]
    fn factory_create_uses_fixed_codex_endpoints() {
        let Some(auth_controller) = auth_controller() else {
            return;
        };
        let factory = OpenAiCodexFactory::new(Arc::new(auth_controller));
        let provider = factory
            .create(ProviderId::new("openai"), DynamicConfig::new())
            .unwrap();

        assert_eq!(provider.get_provider_id(), ProviderId::new("openai"));
    }

    #[test]
    fn resolve_unsuffixed_catalog_model_uses_default_reasoning_effort() {
        let resolved = resolve_request_model(&ModelId::new("gpt-5.2-codex")).unwrap();

        assert_eq!(resolved.api_model, "gpt-5.2-codex");
        assert_eq!(
            resolved.reasoning,
            Some(ResponsesReasoning {
                effort: "medium".to_string(),
            })
        );
    }

    #[test]
    fn resolve_suffixed_catalog_model_uses_requested_reasoning_effort() {
        let resolved = resolve_request_model(&ModelId::new("gpt-5.2-codex-high")).unwrap();

        assert_eq!(resolved.api_model, "gpt-5.2-codex");
        assert_eq!(
            resolved.reasoning,
            Some(ResponsesReasoning {
                effort: "high".to_string(),
            })
        );
    }

    #[test]
    fn resolve_unknown_model_passes_through_unchanged() {
        let resolved = resolve_request_model(&ModelId::new("custom-experimental-model")).unwrap();

        assert_eq!(resolved.api_model, "custom-experimental-model");
        assert_eq!(resolved.reasoning, None);
    }

    #[test]
    fn resolve_invalid_effort_suffix_returns_error() {
        let error = resolve_request_model(&ModelId::new("gpt-5.2-codex-minimal"))
            .expect_err("unsupported effort should fail");

        assert!(
            error
                .to_string()
                .contains("unsupported reasoning effort 'minimal'")
        );
    }

    #[test]
    fn discovered_models_expand_visible_catalog_entries_into_variants() {
        let Some(provider) = provider() else {
            return;
        };
        let discovered = provider.discovered_models(None);
        let ids = discovered
            .into_iter()
            .map(|model| model.id.to_string())
            .collect::<Vec<_>>();

        assert!(ids.contains(&"gpt-5.2-codex-low".to_string()));
        assert!(ids.contains(&"gpt-5.2-codex-medium".to_string()));
        assert!(ids.contains(&"gpt-5.2-codex-high".to_string()));
        assert!(ids.contains(&"gpt-5.2-codex-xhigh".to_string()));
        assert!(!ids.contains(&"gpt-5.1-codex-medium".to_string()));
    }

    #[test]
    fn discovered_models_intersect_remote_availability() {
        let Some(provider) = provider() else {
            return;
        };
        let remote = BTreeMap::from([(
            "gpt-5.2-codex".to_string(),
            RemoteModelMetadata {
                title: Some("ignored".to_string()),
                max_context: Some(111),
            },
        )]);

        let discovered = provider.discovered_models(Some(&remote));
        let ids = discovered
            .into_iter()
            .map(|model| model.id.to_string())
            .collect::<Vec<_>>();

        assert!(ids.contains(&"gpt-5.2-codex-low".to_string()));
        assert!(!ids.contains(&"gpt-5.2-low".to_string()));
    }

    #[test]
    fn discovered_models_fall_back_when_remote_matches_nothing() {
        let Some(provider) = provider() else {
            return;
        };
        let remote = BTreeMap::from([(
            "totally-different-model".to_string(),
            RemoteModelMetadata {
                title: Some("Different".to_string()),
                max_context: Some(123),
            },
        )]);

        let discovered = provider.discovered_models(Some(&remote));
        let ids = discovered
            .into_iter()
            .map(|model| model.id.to_string())
            .collect::<Vec<_>>();

        assert!(ids.contains(&"gpt-5.2-codex-low".to_string()));
        assert!(ids.contains(&"gpt-5.2-low".to_string()));
    }
}
