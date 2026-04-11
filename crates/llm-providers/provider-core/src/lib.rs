#![forbid(unsafe_code)]

mod http_retry;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use color_eyre::Result;
use futures::{future::join_all, stream::BoxStream};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use types::{ChatMessage, ModelId, ProviderId};

pub use http_retry::{
    DEFAULT_HTTP_RETRY_POLICY, HttpRetryPolicy, ProviderRequestContext, ProviderRetryEvent,
    ProviderRetryObserver, send_with_retry,
};

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("Provider not found: {0}")]
    ProviderNotFound(ProviderId),

    #[error("Unknown provider type: {0}")]
    UnknownProviderType(String),

    #[error("Failed to parse config: {0}")]
    ConfigParseError(String),

    #[error("Provider '{0}' not registered when trying to add model")]
    ProviderNotRegistered(ProviderId),

    #[error("Factory already registered for type: {0}")]
    FactoryAlreadyRegistered(String),

    #[error("Invalid config:\n{0}")]
    ConfigValidationError(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DynamicValue {
    String(String),
    Bool(bool),
    Integer(i64),
}

impl DynamicValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Bool(_) | Self::Integer(_) => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            Self::String(_) | Self::Integer(_) => None,
        }
    }

    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            Self::String(_) | Self::Bool(_) => None,
        }
    }
}

impl From<String> for DynamicValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for DynamicValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<bool> for DynamicValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for DynamicValue {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

pub type DynamicConfig = BTreeMap<String, DynamicValue>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldValueKind {
    String,
    SecretString,
    Boolean,
    Integer,
    Url,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub key: String,
    pub label: String,
    pub value_kind: FieldValueKind,
    pub required: bool,
    pub secret: bool,
    pub help_text: Option<String>,
    pub default_value: Option<DynamicValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDefinition {
    pub type_id: String,
    pub display_name: String,
    pub protocol_family: String,
    pub description: String,
    pub provider_fields: Vec<FieldDefinition>,
    pub model_fields: Vec<FieldDefinition>,
    pub supports_model_discovery: bool,
    pub default_provider_id_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

#[derive(Default, Clone)]
pub struct ProviderManager {
    providers: BTreeMap<ProviderId, Arc<dyn Provider>>,
}

#[derive(Default, Clone)]
pub struct ProviderRegistry {
    factories: BTreeMap<String, Arc<FactoryEntry>>,
}

struct FactoryEntry {
    definition: ProviderDefinition,
    create: Arc<ProviderFactoryFn>,
    validate_provider_config: Arc<ValidateConfigFn>,
    validate_model_config: Arc<ValidateConfigFn>,
}

type ProviderFactoryFn =
    dyn Fn(ProviderId, DynamicConfig) -> Result<Box<dyn Provider>, ProviderError> + Send + Sync;
type ValidateConfigFn = dyn Fn(&DynamicConfig) -> Vec<ValidationError> + Send + Sync;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderManagerConfig {
    #[serde(default, rename = "provider")]
    pub providers: Vec<ProviderConfig>,
    #[serde(default, rename = "model")]
    pub models: Vec<ModelConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelConfig {
    pub id: ModelId,
    pub provider_id: ProviderId,
    #[serde(flatten)]
    pub config: DynamicConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: ProviderId,
    #[serde(rename = "type")]
    pub type_id: String,
    #[serde(flatten)]
    pub config: DynamicConfig,
}

pub trait ProviderFactory {
    const TYPE_ID: &'static str;

    fn definition() -> ProviderDefinition;

    fn create(id: ProviderId, config: DynamicConfig) -> Result<Box<dyn Provider>>;

    fn validate_provider_config(_config: &DynamicConfig) -> Vec<ValidationError> {
        Vec::new()
    }

    fn validate_model_config(_config: &DynamicConfig) -> Vec<ValidationError> {
        Vec::new()
    }
}

impl ProviderRegistry {
    pub fn register_factory<F: ProviderFactory + 'static>(&mut self) -> Result<(), ProviderError> {
        let mut definition = F::definition();
        definition.type_id = F::TYPE_ID.to_string();

        self.register_dynamic_factory(
            F::TYPE_ID,
            definition,
            |id, config| {
                F::create(id, config)
                    .map_err(|error| ProviderError::ConfigParseError(error.to_string()))
            },
            F::validate_provider_config,
            F::validate_model_config,
        )
    }

    pub fn register_dynamic_factory<C, VP, VM>(
        &mut self,
        type_id: impl Into<String>,
        mut definition: ProviderDefinition,
        create: C,
        validate_provider_config: VP,
        validate_model_config: VM,
    ) -> Result<(), ProviderError>
    where
        C: Fn(ProviderId, DynamicConfig) -> Result<Box<dyn Provider>, ProviderError>
            + Send
            + Sync
            + 'static,
        VP: Fn(&DynamicConfig) -> Vec<ValidationError> + Send + Sync + 'static,
        VM: Fn(&DynamicConfig) -> Vec<ValidationError> + Send + Sync + 'static,
    {
        let key = type_id.into();
        if self.factories.contains_key(&key) {
            return Err(ProviderError::FactoryAlreadyRegistered(key));
        }

        definition.type_id = key.clone();

        let entry = FactoryEntry {
            definition,
            create: Arc::new(create),
            validate_provider_config: Arc::new(validate_provider_config),
            validate_model_config: Arc::new(validate_model_config),
        };

        self.factories.insert(key, Arc::new(entry));
        Ok(())
    }

    pub fn has_factory(&self, provider_type: &str) -> bool {
        self.factories.contains_key(provider_type)
    }

    pub fn list_definitions(&self) -> Vec<ProviderDefinition> {
        self.factories
            .values()
            .map(|entry| entry.definition.clone())
            .collect()
    }

    pub fn get_definition(&self, type_id: &str) -> Option<ProviderDefinition> {
        self.factories
            .get(type_id)
            .map(|entry| entry.definition.clone())
    }

    pub fn validate_provider_config(
        &self,
        type_id: &str,
        config: &DynamicConfig,
    ) -> Result<Vec<ValidationError>, ProviderError> {
        let entry = self
            .factories
            .get(type_id)
            .ok_or_else(|| ProviderError::UnknownProviderType(type_id.to_string()))?;
        Ok((entry.validate_provider_config)(config))
    }

    pub fn validate_model_config(
        &self,
        type_id: &str,
        config: &DynamicConfig,
    ) -> Result<Vec<ValidationError>, ProviderError> {
        let entry = self
            .factories
            .get(type_id)
            .ok_or_else(|| ProviderError::UnknownProviderType(type_id.to_string()))?;
        Ok((entry.validate_model_config)(config))
    }

    fn create_provider(
        &self,
        type_id: &str,
        id: ProviderId,
        config: DynamicConfig,
    ) -> Result<Box<dyn Provider>, ProviderError> {
        let entry = self
            .factories
            .get(type_id)
            .ok_or_else(|| ProviderError::UnknownProviderType(type_id.to_string()))?;
        (entry.create)(id, config)
    }
}

impl ProviderManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_provider(&mut self, id: ProviderId, provider: Box<dyn Provider>) {
        self.providers.insert(id, Arc::from(provider));
    }

    pub fn has_provider(&self, id: &ProviderId) -> bool {
        self.providers.contains_key(id)
    }

    pub fn get_provider(&self, id: &ProviderId) -> Option<Arc<dyn Provider>> {
        self.providers.get(id).cloned()
    }

    pub fn list_providers(&self) -> Vec<ProviderId> {
        self.providers.keys().cloned().collect()
    }

    pub async fn load_config(
        &mut self,
        config: ProviderManagerConfig,
        registry: ProviderRegistry,
    ) -> Result<()> {
        let mut providers = BTreeMap::new();
        let mut provider_types = BTreeMap::new();

        for provider_config in config.providers {
            let errors = registry
                .validate_provider_config(&provider_config.type_id, &provider_config.config)?;
            if !errors.is_empty() {
                return Err(
                    ProviderError::ConfigValidationError(format_validation_errors(
                        &format!("providers[{}]", provider_config.id),
                        &errors,
                    ))
                    .into(),
                );
            }

            provider_types.insert(provider_config.id.clone(), provider_config.type_id.clone());
            let provider = registry.create_provider(
                &provider_config.type_id,
                provider_config.id.clone(),
                provider_config.config,
            )?;
            providers.insert(provider_config.id, provider);
        }

        for model_config in config.models {
            let provider = providers
                .get_mut(&model_config.provider_id)
                .ok_or_else(|| {
                    ProviderError::ProviderNotRegistered(model_config.provider_id.clone())
                })?;
            let provider_type = provider_types
                .get(&model_config.provider_id)
                .ok_or_else(|| {
                    ProviderError::ProviderNotRegistered(model_config.provider_id.clone())
                })?;
            let errors = registry.validate_model_config(provider_type, &model_config.config)?;
            if !errors.is_empty() {
                return Err(
                    ProviderError::ConfigValidationError(format_validation_errors(
                        &format!("models[{}]", model_config.id),
                        &errors,
                    ))
                    .into(),
                );
            }
            provider.register_model(model_config).await?;
        }

        self.providers = providers
            .into_iter()
            .map(|(id, provider)| (id, Arc::from(provider)))
            .collect();

        self.update_models_list().await?;
        Ok(())
    }

    pub async fn list_all_models(&self) -> HashMap<ProviderId, Vec<Model>> {
        join_all(
            self.providers
                .iter()
                .map(|(id, provider)| async { (id.clone(), provider.list_models().await) })
                .collect::<Vec<_>>(),
        )
        .await
        .into_iter()
        .collect()
    }

    pub async fn update_models_list(&mut self) -> Result<()> {
        for provider in self.providers.values() {
            provider.cache_models().await?;
        }
        Ok(())
    }

    pub async fn generate_reply(
        &self,
        provider_id: ProviderId,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
        request_context: ProviderRequestContext,
    ) -> Result<ChatMessage> {
        let provider = self
            .providers
            .get(&provider_id)
            .ok_or_else(|| ProviderError::ProviderNotFound(provider_id.clone()))?;
        provider
            .generate_reply(model_id, messages, &request_context)
            .await
    }

    pub async fn generate_reply_stream(
        &self,
        provider_id: ProviderId,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
        request_context: ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let provider = self
            .providers
            .get(&provider_id)
            .ok_or_else(|| ProviderError::ProviderNotFound(provider_id.clone()))?;
        provider
            .generate_reply_stream(model_id, messages, &request_context)
            .await
    }
}

fn format_validation_errors(prefix: &str, errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(|error| format!("{prefix}.{}: {}", error.field, error.message))
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn get_provider_id(&self) -> ProviderId;

    async fn list_models(&self) -> Vec<Model>;

    async fn cache_models(&self) -> Result<()>;

    async fn register_model(&mut self, model: ModelConfig) -> Result<()>;

    async fn generate_reply(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
        request_context: &ProviderRequestContext,
    ) -> Result<ChatMessage>;

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
        request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<String>>>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: ModelId,
    pub name: String,
    pub max_context: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockProvider {
        id: ProviderId,
        models: Vec<Model>,
        reply_count: AtomicUsize,
    }

    impl MockProvider {
        fn new(id: &str) -> Self {
            Self {
                id: ProviderId::new(id),
                models: vec![Model {
                    id: ModelId::new("mock-model"),
                    name: "Mock Model".to_string(),
                    max_context: Some(4096),
                }],
                reply_count: AtomicUsize::new(0),
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
            Ok(())
        }

        async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
            Ok(())
        }

        async fn generate_reply(
            &self,
            _model_id: &ModelId,
            messages: Vec<ChatMessage>,
            _request_context: &ProviderRequestContext,
        ) -> Result<ChatMessage> {
            self.reply_count.fetch_add(1, Ordering::SeqCst);
            let last_content = messages.last().map(|m| m.content.as_str()).unwrap_or("");
            Ok(ChatMessage {
                role: types::ChatRole::Assistant,
                content: format!("Response to: {last_content}"),
            })
        }

        async fn generate_reply_stream(
            &self,
            _model_id: &ModelId,
            messages: Vec<ChatMessage>,
            _request_context: &ProviderRequestContext,
        ) -> Result<BoxStream<'static, Result<String>>> {
            use futures::StreamExt;

            self.reply_count.fetch_add(1, Ordering::SeqCst);
            let last_content = messages.last().map(|m| m.content.as_str()).unwrap_or("");
            let response = format!("Streamed response to: {last_content}");
            Ok(futures::stream::iter(vec![Ok(response)]).boxed())
        }
    }

    struct MockFactory;

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

    #[test]
    fn test_registry_registration() {
        let mut registry = ProviderRegistry::default();
        registry.register_factory::<MockFactory>().unwrap();
        assert!(registry.has_factory("mock"));
        assert_eq!(
            registry.get_definition("mock").unwrap().display_name,
            "Mock".to_string()
        );
    }

    #[test]
    fn test_dynamic_registry_registration() {
        let mut registry = ProviderRegistry::default();
        let create_count = Arc::new(AtomicUsize::new(0));
        let create_count_for_factory = Arc::clone(&create_count);

        registry
            .register_dynamic_factory(
                "dynamic-mock",
                ProviderDefinition {
                    type_id: String::new(),
                    display_name: "Dynamic Mock".to_string(),
                    protocol_family: "mock".to_string(),
                    description: "Mock provider built from closures".to_string(),
                    provider_fields: vec![],
                    model_fields: vec![],
                    supports_model_discovery: true,
                    default_provider_id_prefix: "dynamic-mock".to_string(),
                },
                move |id, _config| {
                    create_count_for_factory.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::new(MockProvider::new(id.as_str())))
                },
                |_| Vec::new(),
                |_| Vec::new(),
            )
            .unwrap();

        let provider = registry
            .create_provider(
                "dynamic-mock",
                ProviderId::new("dynamic-mock"),
                DynamicConfig::new(),
            )
            .unwrap();
        assert_eq!(provider.get_provider_id(), ProviderId::new("dynamic-mock"));
        assert_eq!(create_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_dynamic_registry_rejects_duplicates() {
        let mut registry = ProviderRegistry::default();
        registry
            .register_dynamic_factory(
                "duplicate",
                ProviderDefinition {
                    type_id: String::new(),
                    display_name: "Duplicate".to_string(),
                    protocol_family: "mock".to_string(),
                    description: "duplicate".to_string(),
                    provider_fields: vec![],
                    model_fields: vec![],
                    supports_model_discovery: false,
                    default_provider_id_prefix: "duplicate".to_string(),
                },
                |id, _config| Ok(Box::new(MockProvider::new(id.as_str()))),
                |_| Vec::new(),
                |_| Vec::new(),
            )
            .unwrap();

        let error = registry
            .register_dynamic_factory(
                "duplicate",
                ProviderDefinition {
                    type_id: String::new(),
                    display_name: "Duplicate".to_string(),
                    protocol_family: "mock".to_string(),
                    description: "duplicate".to_string(),
                    provider_fields: vec![],
                    model_fields: vec![],
                    supports_model_discovery: false,
                    default_provider_id_prefix: "duplicate".to_string(),
                },
                |id, _config| Ok(Box::new(MockProvider::new(id.as_str()))),
                |_| Vec::new(),
                |_| Vec::new(),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            ProviderError::FactoryAlreadyRegistered(provider_type) if provider_type == "duplicate"
        ));
    }

    #[tokio::test]
    async fn test_load_config() {
        let mut registry = ProviderRegistry::default();
        registry.register_factory::<MockFactory>().unwrap();

        let mut config = DynamicConfig::new();
        config.insert("token".to_string(), DynamicValue::from("abc"));

        let mut manager = ProviderManager::new();
        manager
            .load_config(
                ProviderManagerConfig {
                    providers: vec![ProviderConfig {
                        id: ProviderId::new("mock"),
                        type_id: "mock".to_string(),
                        config,
                    }],
                    models: vec![],
                },
                registry,
            )
            .await
            .unwrap();

        assert!(manager.has_provider(&ProviderId::new("mock")));
    }

    #[tokio::test]
    async fn test_invalid_config() {
        let mut registry = ProviderRegistry::default();
        registry.register_factory::<MockFactory>().unwrap();

        let result = ProviderManager::new()
            .load_config(
                ProviderManagerConfig {
                    providers: vec![ProviderConfig {
                        id: ProviderId::new("mock"),
                        type_id: "mock".to_string(),
                        config: DynamicConfig::new(),
                    }],
                    models: vec![],
                },
                registry,
            )
            .await;

        assert!(result.is_err());
    }
}
