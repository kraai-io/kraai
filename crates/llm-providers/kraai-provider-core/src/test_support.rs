use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use color_eyre::{Result, eyre::eyre};
use futures::{StreamExt, stream::BoxStream};
use kraai_types::{AssistantPhase, ModelId, ProviderId};

use crate::config::{DynamicConfig, DynamicValue, ModelConfig};
use crate::definition::{FieldDefinition, FieldValueKind, ProviderDefinition, ValidationError};
use crate::http_retry::ProviderRequestContext;
use crate::provider::{Model, Provider, ProviderRequest};
use crate::registry::ProviderFactory;
use crate::stream::ProviderStreamEvent;

pub(crate) struct MockProvider {
    id: ProviderId,
    models: Vec<Model>,
    reply_count: AtomicUsize,
    pub(crate) cache_count: AtomicUsize,
    fail_cache: bool,
    cache_delay: Option<Duration>,
}

impl MockProvider {
    pub(crate) fn new(id: &str) -> Self {
        Self {
            id: ProviderId::new(id),
            models: vec![Model {
                id: ModelId::new("mock-model"),
                name: "Mock Model".to_string(),
                max_context: Some(4096),
            }],
            reply_count: AtomicUsize::new(0),
            cache_count: AtomicUsize::new(0),
            fail_cache: false,
            cache_delay: None,
        }
    }

    pub(crate) fn failing_cache(id: &str) -> Self {
        Self {
            fail_cache: true,
            ..Self::new(id)
        }
    }
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<Model> {
        self.models.clone()
    }

    async fn cache_models(&self) -> Result<()> {
        self.cache_count.fetch_add(1, Ordering::SeqCst);
        if let Some(delay) = self.cache_delay {
            tokio::time::sleep(delay).await;
        }
        if self.fail_cache {
            return Err(eyre!("cache failed for {}", self.id));
        }
        Ok(())
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &ModelId,
        request: ProviderRequest,
        _request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        self.reply_count.fetch_add(1, Ordering::SeqCst);
        let last_content = request
            .messages
            .last()
            .map(kraai_types::ConversationItem::display_text)
            .unwrap_or_default();
        let response = format!("Streamed response to: {last_content}");
        Ok(
            futures::stream::iter(vec![Ok(ProviderStreamEvent::TextDelta {
                item_id: String::from("mock-message"),
                phase: AssistantPhase::FinalAnswer,
                delta: response,
            })])
            .boxed(),
        )
    }
}

pub(crate) struct MockFactory;

impl ProviderFactory for MockFactory {
    const TYPE_ID: &'static str = "mock";

    fn definition() -> ProviderDefinition {
        ProviderDefinition {
            type_id: String::new(),
            display_name: "Mock".to_string(),
            protocol_family: "mock".to_string(),
            description: "Mock provider".to_string(),
            provider_fields: vec![FieldDefinition {
                key: "token".to_string(),
                label: "Token".to_string(),
                value_kind: FieldValueKind::String,
                required: true,
                secret: false,
                help_text: None,
                default_value: None,
            }],
            model_fields: vec![],
            supports_model_discovery: true,
            default_provider_id_prefix: "mock".to_string(),
        }
    }

    fn create(id: ProviderId, _config: DynamicConfig) -> Result<Box<dyn Provider>> {
        Ok(Box::new(MockProvider::new(id.as_str())))
    }

    fn validate_provider_config(config: &DynamicConfig) -> Vec<ValidationError> {
        if config.get("token").and_then(DynamicValue::as_str).is_none() {
            vec![ValidationError {
                field: "token".to_string(),
                message: "token is required".to_string(),
            }]
        } else {
            Vec::new()
        }
    }
}

pub(crate) fn simple_provider_definition(
    display_name: &str,
    description: &str,
    supports_model_discovery: bool,
    default_provider_id_prefix: &str,
) -> ProviderDefinition {
    ProviderDefinition {
        type_id: String::new(),
        display_name: display_name.to_string(),
        protocol_family: "mock".to_string(),
        description: description.to_string(),
        provider_fields: vec![],
        model_fields: vec![],
        supports_model_discovery,
        default_provider_id_prefix: default_provider_id_prefix.to_string(),
    }
}
