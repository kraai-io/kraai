#![forbid(unsafe_code)]

mod http_retry;
mod sse;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use color_eyre::Result;
use futures::{future::join_all, stream::BoxStream};
use kraai_types::{ChatMessage, ModelId, ProviderId, TokenUsage};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use http_retry::{
    DEFAULT_HTTP_RETRY_POLICY, HttpRetryPolicy, ProviderRequestContext, ProviderRetryEvent,
    ProviderRetryObserver, send_with_retry,
};
pub use sse::stream_sse_data;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderStreamEvent {
    TextDelta(String),
    Usage(TokenUsage),
}

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

    #[error("Failed to refresh model cache for provider(s): {0:?}")]
    ModelCacheRefreshFailed(Vec<ProviderModelCacheRefreshError>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelCacheRefreshError {
    pub provider_id: ProviderId,
    pub message: String,
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
    const PROVIDER_INITIALIZATION_CONCURRENCY: usize = 8;
    const MODEL_CACHE_REFRESH_CONCURRENCY: usize = 8;

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
        let mut provider_types = BTreeMap::new();
        let mut provider_configs = BTreeMap::new();
        let mut models_by_provider: BTreeMap<ProviderId, Vec<ModelConfig>> = BTreeMap::new();

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
            provider_configs.insert(provider_config.id.clone(), provider_config);
        }

        for model_config in config.models {
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
            models_by_provider
                .entry(model_config.provider_id.clone())
                .or_default()
                .push(model_config);
        }

        let mut provider_configs = provider_configs.into_iter();
        let mut providers = BTreeMap::new();
        let mut failures = Vec::new();
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            while tasks.len() < Self::PROVIDER_INITIALIZATION_CONCURRENCY {
                let Some((provider_id, provider_config)) = provider_configs.next() else {
                    break;
                };
                let registry = registry.clone();
                let models = models_by_provider.remove(&provider_id).unwrap_or_default();
                tasks.spawn(async move {
                    let mut provider = registry.create_provider(
                        &provider_config.type_id,
                        provider_config.id.clone(),
                        provider_config.config,
                    )?;
                    for model in models {
                        provider.register_model(model).await?;
                    }
                    Ok::<_, color_eyre::Report>((provider_id, provider))
                });
            }

            let Some(result) = tasks.join_next().await else {
                break;
            };
            match result {
                Ok(Ok((provider_id, provider))) => {
                    providers.insert(provider_id, provider);
                }
                Ok(Err(error)) => failures.push(error.to_string()),
                Err(error) => failures.push(error.to_string()),
            }
        }

        if !failures.is_empty() {
            return Err(ProviderError::ConfigValidationError(failures.join("\n")).into());
        }

        let providers = providers
            .into_iter()
            .map(|(id, provider)| (id, Arc::from(provider)))
            .collect();

        self.providers = providers;
        self.update_models_list().await
    }

    pub async fn list_all_models(&self) -> HashMap<ProviderId, Vec<Model>> {
        join_all(
            self.providers
                .iter()
                .map(|(id, provider)| async { (id.clone(), provider.list_models().await) }),
        )
        .await
        .into_iter()
        .collect()
    }

    pub async fn update_models_list(&mut self) -> Result<()> {
        Self::update_models_list_for(&self.providers).await
    }

    async fn update_models_list_for(
        providers: &BTreeMap<ProviderId, Arc<dyn Provider>>,
    ) -> Result<()> {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(
            Self::MODEL_CACHE_REFRESH_CONCURRENCY,
        ));
        let mut tasks = tokio::task::JoinSet::new();

        for (provider_id, provider) in providers {
            let provider_id = provider_id.clone();
            let provider = provider.clone();
            let semaphore = semaphore.clone();

            tasks.spawn(async move {
                let _permit = semaphore.acquire_owned().await.map_err(|error| {
                    ProviderModelCacheRefreshError {
                        provider_id: provider_id.clone(),
                        message: format!("Failed to acquire model cache refresh permit: {error}"),
                    }
                })?;

                provider
                    .cache_models()
                    .await
                    .map_err(|error| ProviderModelCacheRefreshError {
                        provider_id,
                        message: error.to_string(),
                    })
            });
        }

        let mut failures = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(error),
                Err(error) => failures.push(ProviderModelCacheRefreshError {
                    provider_id: ProviderId::new("<unknown>"),
                    message: format!("Model cache refresh task failed: {error}"),
                }),
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(ProviderError::ModelCacheRefreshFailed(failures).into())
        }
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
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
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
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: ModelId,
    pub name: String,
    pub max_context: Option<usize>,
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible provider setup is combined with direct assertions"
)]
mod tests {
    use super::*;
    use color_eyre::eyre::eyre;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    struct MockProvider {
        id: ProviderId,
        models: Vec<Model>,
        reply_count: AtomicUsize,
        cache_count: AtomicUsize,
        fail_cache: bool,
        cache_delay: Option<Duration>,
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
                cache_count: AtomicUsize::new(0),
                fail_cache: false,
                cache_delay: None,
            }
        }

        fn failing_cache(id: &str) -> Self {
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

        async fn generate_reply(
            &self,
            _model_id: &ModelId,
            messages: Vec<ChatMessage>,
            _request_context: &ProviderRequestContext,
        ) -> Result<ChatMessage> {
            self.reply_count.fetch_add(1, Ordering::SeqCst);
            let last_content = messages.last().map(|m| m.content.as_str()).unwrap_or("");
            Ok(ChatMessage {
                role: kraai_types::ChatRole::Assistant,
                content: format!("Response to: {last_content}"),
            })
        }

        async fn generate_reply_stream(
            &self,
            _model_id: &ModelId,
            messages: Vec<ChatMessage>,
            _request_context: &ProviderRequestContext,
        ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
            use futures::StreamExt;

            self.reply_count.fetch_add(1, Ordering::SeqCst);
            let last_content = messages.last().map(|m| m.content.as_str()).unwrap_or("");
            let response = format!("Streamed response to: {last_content}");
            Ok(futures::stream::iter(vec![Ok(ProviderStreamEvent::TextDelta(response))]).boxed())
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
    fn test_registry_registration() -> Result<()> {
        let mut registry = ProviderRegistry::default();
        registry.register_factory::<MockFactory>()?;
        assert!(registry.has_factory("mock"));
        assert_eq!(
            registry
                .get_definition("mock")
                .ok_or_else(|| eyre!("mock factory definition missing"))?
                .display_name,
            "Mock".to_string()
        );
        Ok(())
    }

    #[test]
    fn test_dynamic_registry_registration() -> Result<()> {
        let mut registry = ProviderRegistry::default();
        let create_count = Arc::new(AtomicUsize::new(0));
        let create_count_for_factory = Arc::clone(&create_count);

        registry.register_dynamic_factory(
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
        )?;

        let provider = registry.create_provider(
            "dynamic-mock",
            ProviderId::new("dynamic-mock"),
            DynamicConfig::new(),
        )?;
        assert_eq!(provider.get_provider_id(), ProviderId::new("dynamic-mock"));
        assert_eq!(create_count.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn test_dynamic_registry_rejects_duplicates() -> Result<()> {
        let mut registry = ProviderRegistry::default();
        registry.register_dynamic_factory(
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
        )?;

        let result = registry.register_dynamic_factory(
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
        );
        let Err(error) = result else {
            return Err(eyre!("duplicate factory registration succeeded"));
        };

        assert!(matches!(
            error,
            ProviderError::FactoryAlreadyRegistered(provider_type) if provider_type == "duplicate"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_load_config() -> Result<()> {
        let mut registry = ProviderRegistry::default();
        registry.register_factory::<MockFactory>()?;

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
            .await?;

        assert!(manager.has_provider(&ProviderId::new("mock")));
        Ok(())
    }

    #[tokio::test]
    async fn load_config_bounds_provider_initialization_concurrency() -> Result<()> {
        struct ObservedProvider {
            id: ProviderId,
            active_initializations: Arc<AtomicUsize>,
            peak_initializations: Arc<AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl Provider for ObservedProvider {
            fn get_provider_id(&self) -> ProviderId {
                self.id.clone()
            }

            async fn list_models(&self) -> Vec<Model> {
                Vec::new()
            }

            async fn cache_models(&self) -> Result<()> {
                Ok(())
            }

            async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
                let active = self.active_initializations.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak_initializations
                    .fetch_max(active, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(25)).await;
                self.active_initializations.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            }

            async fn generate_reply(
                &self,
                _model_id: &ModelId,
                _messages: Vec<ChatMessage>,
                _request_context: &ProviderRequestContext,
            ) -> Result<ChatMessage> {
                unreachable!("not used by this test")
            }

            async fn generate_reply_stream(
                &self,
                _model_id: &ModelId,
                _messages: Vec<ChatMessage>,
                _request_context: &ProviderRequestContext,
            ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
                unreachable!("not used by this test")
            }
        }

        let active_initializations = Arc::new(AtomicUsize::new(0));
        let peak_initializations = Arc::new(AtomicUsize::new(0));
        let mut registry = ProviderRegistry::default();
        registry.register_dynamic_factory(
            "observed",
            ProviderDefinition {
                type_id: String::new(),
                display_name: "Observed".to_string(),
                protocol_family: "mock".to_string(),
                description: "Records initialization concurrency".to_string(),
                provider_fields: vec![],
                model_fields: vec![],
                supports_model_discovery: false,
                default_provider_id_prefix: "observed".to_string(),
            },
            {
                let active_initializations = active_initializations.clone();
                let peak_initializations = peak_initializations.clone();
                move |id, _config| {
                    Ok(Box::new(ObservedProvider {
                        id,
                        active_initializations: active_initializations.clone(),
                        peak_initializations: peak_initializations.clone(),
                    }))
                }
            },
            |_| Vec::new(),
            |_| Vec::new(),
        )?;

        let provider_count = ProviderManager::PROVIDER_INITIALIZATION_CONCURRENCY + 2;
        let config = ProviderManagerConfig {
            providers: (0..provider_count)
                .map(|index| ProviderConfig {
                    id: ProviderId::new(format!("provider-{index}")),
                    type_id: "observed".to_string(),
                    config: DynamicConfig::new(),
                })
                .collect(),
            models: (0..provider_count)
                .map(|index| ModelConfig {
                    id: ModelId::new(format!("model-{index}")),
                    provider_id: ProviderId::new(format!("provider-{index}")),
                    config: DynamicConfig::new(),
                })
                .collect(),
        };

        ProviderManager::new().load_config(config, registry).await?;

        assert_eq!(
            peak_initializations.load(Ordering::SeqCst),
            ProviderManager::PROVIDER_INITIALIZATION_CONCURRENCY
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_invalid_config() -> Result<()> {
        let mut registry = ProviderRegistry::default();
        registry.register_factory::<MockFactory>()?;

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
        Ok(())
    }

    #[tokio::test]
    async fn load_config_preserves_active_providers_when_creation_fails() -> Result<()> {
        let mut registry = ProviderRegistry::default();
        registry.register_factory::<MockFactory>()?;
        registry.register_dynamic_factory(
            "failing",
            ProviderDefinition {
                type_id: String::new(),
                display_name: "Failing".to_string(),
                protocol_family: "mock".to_string(),
                description: "Fails during creation".to_string(),
                provider_fields: vec![],
                model_fields: vec![],
                supports_model_discovery: false,
                default_provider_id_prefix: "failing".to_string(),
            },
            |_id, _config| {
                Err(ProviderError::ConfigParseError(
                    "provider creation failed".to_string(),
                ))
            },
            |_| Vec::new(),
            |_| Vec::new(),
        )?;

        let mut valid_config = DynamicConfig::new();
        valid_config.insert("token".to_string(), DynamicValue::from("abc"));

        let mut manager = ProviderManager::new();
        manager.register_provider(
            ProviderId::new("active"),
            Box::new(MockProvider::new("active")),
        );

        let result = manager
            .load_config(
                ProviderManagerConfig {
                    providers: vec![
                        ProviderConfig {
                            id: ProviderId::new("valid"),
                            type_id: "mock".to_string(),
                            config: valid_config,
                        },
                        ProviderConfig {
                            id: ProviderId::new("invalid"),
                            type_id: "failing".to_string(),
                            config: DynamicConfig::new(),
                        },
                    ],
                    models: vec![],
                },
                registry,
            )
            .await;

        assert!(result.is_err());
        assert_eq!(manager.list_providers(), vec![ProviderId::new("active")]);
        Ok(())
    }

    #[tokio::test]
    async fn load_config_replaces_active_providers_when_model_cache_refresh_fails() -> Result<()> {
        let mut registry = ProviderRegistry::default();
        registry.register_dynamic_factory(
            "failing-cache",
            ProviderDefinition {
                type_id: String::new(),
                display_name: "Failing Cache".to_string(),
                protocol_family: "mock".to_string(),
                description: "Fails while refreshing its model cache".to_string(),
                provider_fields: vec![],
                model_fields: vec![],
                supports_model_discovery: true,
                default_provider_id_prefix: "failing-cache".to_string(),
            },
            |id, _config| Ok(Box::new(MockProvider::failing_cache(id.as_str()))),
            |_| Vec::new(),
            |_| Vec::new(),
        )?;

        let mut manager = ProviderManager::new();
        manager.register_provider(
            ProviderId::new("active"),
            Box::new(MockProvider::new("active")),
        );

        let result = manager
            .load_config(
                ProviderManagerConfig {
                    providers: vec![ProviderConfig {
                        id: ProviderId::new("replacement"),
                        type_id: "failing-cache".to_string(),
                        config: DynamicConfig::new(),
                    }],
                    models: vec![],
                },
                registry,
            )
            .await;

        assert!(result.is_err());
        assert_eq!(
            manager.list_providers(),
            vec![ProviderId::new("replacement")]
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_models_list_attempts_every_provider_before_returning_failures() -> Result<()> {
        let failing = Arc::new(MockProvider::failing_cache("failing"));
        let successful = Arc::new(MockProvider::new("successful"));

        let mut manager = ProviderManager::new();
        manager
            .providers
            .insert(ProviderId::new("failing"), failing.clone());
        manager
            .providers
            .insert(ProviderId::new("successful"), successful.clone());

        let result = manager.update_models_list().await;
        let Err(error) = result else {
            return Err(eyre!("model cache refresh unexpectedly succeeded"));
        };

        assert_eq!(failing.cache_count.load(Ordering::SeqCst), 1);
        assert_eq!(successful.cache_count.load(Ordering::SeqCst), 1);

        let refresh_error = error
            .downcast_ref::<ProviderError>()
            .ok_or_else(|| eyre!("expected provider error"))?;
        assert!(matches!(
            refresh_error,
            ProviderError::ModelCacheRefreshFailed(failures)
                if failures == &vec![ProviderModelCacheRefreshError {
                    provider_id: ProviderId::new("failing"),
                    message: "cache failed for failing".to_string(),
                }]
        ));
        Ok(())
    }

    #[tokio::test]
    async fn update_models_list_refreshes_providers_concurrently() -> Result<()> {
        struct ObservedProvider {
            id: ProviderId,
            active: Arc<AtomicUsize>,
            overlap_seen: Arc<AtomicBool>,
        }

        #[async_trait::async_trait]
        impl Provider for ObservedProvider {
            fn get_provider_id(&self) -> ProviderId {
                self.id.clone()
            }

            async fn list_models(&self) -> Vec<Model> {
                Vec::new()
            }

            async fn cache_models(&self) -> Result<()> {
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                if active > 1 {
                    self.overlap_seen.store(true, Ordering::SeqCst);
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
                self.active.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            }

            async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
                Ok(())
            }

            async fn generate_reply(
                &self,
                _model_id: &ModelId,
                _messages: Vec<ChatMessage>,
                _request_context: &ProviderRequestContext,
            ) -> Result<ChatMessage> {
                unreachable!("not used by this test")
            }

            async fn generate_reply_stream(
                &self,
                _model_id: &ModelId,
                _messages: Vec<ChatMessage>,
                _request_context: &ProviderRequestContext,
            ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
                unreachable!("not used by this test")
            }
        }

        let active = Arc::new(AtomicUsize::new(0));
        let overlap_seen = Arc::new(AtomicBool::new(false));
        let mut manager = ProviderManager::new();
        for id in ["first", "second"] {
            manager.providers.insert(
                ProviderId::new(id),
                Arc::new(ObservedProvider {
                    id: ProviderId::new(id),
                    active: active.clone(),
                    overlap_seen: overlap_seen.clone(),
                }),
            );
        }

        manager.update_models_list().await?;

        assert!(overlap_seen.load(Ordering::SeqCst));
        Ok(())
    }
}
