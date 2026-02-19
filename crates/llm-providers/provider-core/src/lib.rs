//! Provider core crate for LLM provider abstraction.
//!
//! This crate provides the core traits and types for managing LLM providers
//! in a provider-agnostic way. It supports:
//!
//! - Dynamic provider registration via factory pattern
//! - Configuration-driven provider loading
//! - Streaming and non-streaming chat completions
//! - Model caching and discovery

use std::collections::{BTreeMap, HashMap};

use color_eyre::Result;
use futures::{future::join_all, stream::BoxStream};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use types::{ChatMessage, ModelId, ProviderId};

/// Errors that can occur when working with providers.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// The requested provider was not found in the manager.
    #[error("Provider not found: {0}")]
    ProviderNotFound(ProviderId),

    /// An unknown provider type was specified in configuration.
    #[error("Unknown provider type: {0}")]
    UnknownProviderType(String),

    /// Failed to parse provider or model configuration.
    #[error("Failed to parse config: {0}")]
    ConfigParseError(String),

    /// Attempted to register a model for a provider that doesn't exist.
    #[error("Provider '{0}' not registered when trying to add model")]
    ProviderNotRegistered(ProviderId),

    /// Attempted to register a factory for a type that already has one.
    #[error("Factory already registered for type: {0}")]
    FactoryAlreadyRegistered(String),
}

/// Manages a collection of LLM providers.
///
/// Use [`ProviderManager`] to:
/// - Store and retrieve providers by ID
/// - Generate chat completions (streaming or non-streaming)
/// - List available models across all providers
///
/// # Example
///
/// ```ignore
/// let mut manager = ProviderManager::new();
/// manager.register_provider(ProviderId::new("my-provider"), my_provider);
/// let models = manager.list_all_models().await;
/// ```
#[derive(Default)]
pub struct ProviderManager {
    providers: BTreeMap<ProviderId, Box<dyn Provider>>,
}

/// Helper for registering provider factories before loading configuration.
///
/// The factory pattern allows providers to be created dynamically from
/// configuration files. Register factories for each provider type you support,
/// then load configuration to instantiate them.
///
/// # Example
///
/// ```ignore
/// let mut helper = ProviderManagerHelper::default();
/// helper.register_factory::<OpenAIFactory>();
/// helper.register_factory::<GoogleFactory>();
///
/// let mut manager = ProviderManager::new();
/// manager.load_config(config, helper).await?;
/// ```
#[derive(Default)]
pub struct ProviderManagerHelper {
    factory_registry: BTreeMap<String, ProviderFactoryFn>,
}

type ProviderFactoryFn = Box<dyn Fn(ProviderId, toml::Value) -> Result<Box<dyn Provider>, ProviderError> + Send>;

/// Configuration for the provider manager.
///
/// This struct is typically deserialized from a TOML configuration file.
#[derive(Deserialize, Serialize)]
pub struct ProviderManagerConfig {
    /// List of providers to configure.
    #[serde(default, rename = "provider")]
    pub providers: Vec<ProviderConfig>,
    /// List of models to register with specific providers.
    #[serde(default, rename = "model")]
    pub models: Vec<ModelConfig>,
}

/// Configuration for a specific model.
#[derive(Deserialize, Serialize)]
pub struct ModelConfig {
    /// The provider this model belongs to.
    pub provider_id: ProviderId,
    /// Additional model-specific configuration.
    #[serde(flatten)]
    pub config: toml::Value,
}

/// Configuration for a provider instance.
#[derive(Deserialize, Serialize)]
pub struct ProviderConfig {
    /// Unique identifier for this provider instance.
    pub id: ProviderId,
    /// Type of provider (e.g., "openai", "google").
    pub r#type: String,
    /// Provider-specific configuration.
    #[serde(flatten)]
    pub config: toml::Value,
}

/// Factory trait for creating provider instances.
///
/// Implement this trait for each provider type to enable
/// configuration-driven provider creation.
///
/// # Example
///
/// ```ignore
/// pub struct OpenAIFactory;
///
/// impl ProviderFactory for OpenAIFactory {
///     const TYPE: &'static str = "openai";
///     type Config = OpenAIConfig;
///
///     fn create(id: ProviderId, config: Self::Config) -> Result<Box<dyn Provider>> {
///         Ok(Box::new(OpenAIProvider::new(id, config)))
///     }
/// }
/// ```
pub trait ProviderFactory {
    /// Unique string identifier for this provider type.
    const TYPE: &'static str;

    /// Configuration type for this provider.
    type Config: for<'de> Deserialize<'de>;

    /// Create a new provider instance.
    fn create(id: ProviderId, config: Self::Config) -> Result<Box<dyn Provider>>;
}

impl ProviderManagerHelper {
    /// Register a factory for a provider type.
    ///
    /// # Errors
    ///
    /// Returns an error if a factory is already registered for this type.
    pub fn register_factory<F: ProviderFactory + 'static>(&mut self) -> Result<(), ProviderError> {
        let key = F::TYPE.to_string();
        if self.factory_registry.contains_key(&key) {
            return Err(ProviderError::FactoryAlreadyRegistered(key));
        }
        let factory_fn = |id, config: toml::Value| -> Result<Box<dyn Provider>, ProviderError> {
            let config: F::Config = config
                .try_into()
                .map_err(|e| ProviderError::ConfigParseError(e.to_string()))?;
            F::create(id, config).map_err(|e| ProviderError::ConfigParseError(e.to_string()))
        };
        self.factory_registry.insert(key, Box::new(factory_fn));
        Ok(())
    }

    /// Check if a factory is registered for a given provider type.
    pub fn has_factory(&self, provider_type: &str) -> bool {
        self.factory_registry.contains_key(provider_type)
    }
}

impl ProviderManager {
    /// Create a new empty provider manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider directly.
    ///
    /// Use this for programmatic provider registration without configuration files.
    pub fn register_provider(&mut self, id: ProviderId, provider: Box<dyn Provider>) {
        self.providers.insert(id, provider);
    }

    /// Check if a provider with the given ID exists.
    pub fn has_provider(&self, id: &ProviderId) -> bool {
        self.providers.contains_key(id)
    }

    /// Get a reference to a provider by ID.
    pub fn get_provider(&self, id: &ProviderId) -> Option<&dyn Provider> {
        self.providers.get(id).map(|p| p.as_ref())
    }

    /// List all registered provider IDs.
    pub fn list_providers(&self) -> Vec<ProviderId> {
        self.providers.keys().cloned().collect()
    }

    /// Load providers from configuration.
    ///
    /// This clears any existing providers and creates new ones from the configuration.
    /// Models are registered with their respective providers after creation.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - A provider type is unknown (no factory registered)
    /// - Provider configuration is invalid
    /// - A model references a non-existent provider
    pub async fn load_config(
        &mut self,
        config: ProviderManagerConfig,
        helper: ProviderManagerHelper,
    ) -> Result<()> {
        let providers = config
            .providers
            .into_iter()
            .map(|x| {
                let factory = helper
                    .factory_registry
                    .get(&x.r#type)
                    .ok_or_else(|| ProviderError::UnknownProviderType(x.r#type.clone()))?;
                let provider = factory(x.id.clone(), x.config)?;
                Ok((x.id, provider))
            })
            .collect::<Result<Vec<(ProviderId, Box<dyn Provider>)>, ProviderError>>()?;

        self.providers.clear();
        self.providers.extend(providers);

        for m in config.models {
            let provider = self
                .providers
                .get_mut(&m.provider_id)
                .ok_or_else(|| ProviderError::ProviderNotRegistered(m.provider_id.clone()))?;
            provider.register_model(m).await?;
        }

        self.update_models_list().await?;

        Ok(())
    }

    /// List all models across all providers.
    ///
    /// Returns a map from provider ID to the list of models available for that provider.
    pub async fn list_all_models(&self) -> HashMap<ProviderId, Vec<Model>> {
        join_all(
            self.providers
                .iter()
                .map(|(id, x)| async { (id.clone(), x.list_models().await) })
                .collect::<Vec<_>>(),
        )
        .await
        .into_iter()
        .collect()
    }

    /// Update the cached model list for all providers.
    ///
    /// This queries each provider's API to refresh the list of available models.
    pub async fn update_models_list(&mut self) -> Result<()> {
        for p in self.providers.values_mut() {
            p.cache_models().await?;
        }
        Ok(())
    }

    /// Generate a chat completion (non-streaming).
    ///
    /// # Errors
    ///
    /// Returns an error if the provider is not found.
    pub async fn generate_reply(
        &self,
        provider_id: ProviderId,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let client = self
            .providers
            .get(&provider_id)
            .ok_or_else(|| ProviderError::ProviderNotFound(provider_id.clone()))?;
        client.generate_reply(model_id, messages).await
    }

    /// Generate a streaming chat completion.
    ///
    /// Returns a stream of string chunks that can be collected or processed incrementally.
    ///
    /// # Errors
    ///
    /// Returns an error if the provider is not found.
    pub async fn generate_reply_stream(
        &self,
        provider_id: ProviderId,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let client = self
            .providers
            .get(&provider_id)
            .ok_or_else(|| ProviderError::ProviderNotFound(provider_id.clone()))?;
        client.generate_reply_stream(model_id, messages).await
    }
}

/// Trait for LLM provider implementations.
///
/// Implement this trait to add support for a new LLM provider.
/// The trait handles model discovery, registration, and chat completion.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Get the unique identifier for this provider instance.
    fn get_provider_id(&self) -> ProviderId;

    /// List all available models for this provider.
    async fn list_models(&self) -> Vec<Model>;

    /// Cache the list of models from the provider's API.
    async fn cache_models(&self) -> Result<()>;

    /// Register a model configuration with this provider.
    async fn register_model(&mut self, model: ModelConfig) -> Result<()>;

    /// Generate a chat completion (non-streaming).
    async fn generate_reply(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage>;

    /// Generate a streaming chat completion.
    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<BoxStream<'static, Result<String>>>;
}

/// Information about an LLM model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    /// Unique model identifier.
    pub id: ModelId,
    /// Human-readable model name.
    pub name: String,
    /// Maximum context length in tokens, if known.
    pub max_context: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock provider for testing
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
        ) -> Result<ChatMessage> {
            self.reply_count.fetch_add(1, Ordering::SeqCst);
            let last_content = messages.last().map(|m| m.content.as_str()).unwrap_or("");
            Ok(ChatMessage {
                role: types::ChatRole::Assistant,
                content: format!("Response to: {}", last_content),
            })
        }

        async fn generate_reply_stream(
            &self,
            _model_id: &ModelId,
            messages: Vec<ChatMessage>,
        ) -> Result<BoxStream<'static, Result<String>>> {
            use futures::StreamExt;
            self.reply_count.fetch_add(1, Ordering::SeqCst);
            let last_content = messages.last().map(|m| m.content.as_str()).unwrap_or("");
            let response = format!("Streamed response to: {}", last_content);
            Ok(futures::stream::iter(vec![Ok(response)]).boxed())
        }
    }

    struct MockFactory;

    impl ProviderFactory for MockFactory {
        const TYPE: &'static str = "mock";

        type Config = MockConfig;

        fn create(id: ProviderId, _config: Self::Config) -> Result<Box<dyn Provider>> {
            Ok(Box::new(MockProvider::new(id.as_str())))
        }
    }

    #[derive(Deserialize)]
    struct MockConfig {}

    #[test]
    fn test_provider_manager_new() {
        let manager = ProviderManager::new();
        assert!(!manager.has_provider(&ProviderId::new("test")));
        assert!(manager.list_providers().is_empty());
    }

    #[test]
    fn test_register_provider() {
        let mut manager = ProviderManager::new();
        let id = ProviderId::new("mock");
        manager.register_provider(id.clone(), Box::new(MockProvider::new("mock")));

        assert!(manager.has_provider(&id));
        assert_eq!(manager.list_providers(), vec![id]);
    }

    #[test]
    fn test_get_provider() {
        let mut manager = ProviderManager::new();
        let id = ProviderId::new("mock");
        manager.register_provider(id.clone(), Box::new(MockProvider::new("mock")));

        let provider = manager.get_provider(&id);
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().get_provider_id(), id);

        let missing = manager.get_provider(&ProviderId::new("missing"));
        assert!(missing.is_none());
    }

    #[test]
    fn test_factory_registration() {
        let mut helper = ProviderManagerHelper::default();

        assert!(!helper.has_factory("mock"));
        helper.register_factory::<MockFactory>().unwrap();
        assert!(helper.has_factory("mock"));
    }

    #[test]
    fn test_duplicate_factory_registration() {
        let mut helper = ProviderManagerHelper::default();

        helper.register_factory::<MockFactory>().unwrap();
        let result = helper.register_factory::<MockFactory>();

        assert!(matches!(result, Err(ProviderError::FactoryAlreadyRegistered(_))));
    }

    #[tokio::test]
    async fn test_list_all_models() {
        let mut manager = ProviderManager::new();
        let id = ProviderId::new("mock");
        manager.register_provider(id.clone(), Box::new(MockProvider::new("mock")));

        let models = manager.list_all_models().await;
        assert!(models.contains_key(&id));
        assert_eq!(models[&id].len(), 1);
        assert_eq!(models[&id][0].id.as_str(), "mock-model");
    }

    #[tokio::test]
    async fn test_generate_reply() {
        let mut manager = ProviderManager::new();
        let id = ProviderId::new("mock");
        manager.register_provider(id.clone(), Box::new(MockProvider::new("mock")));

        let messages = vec![ChatMessage {
            role: types::ChatRole::User,
            content: "Hello".to_string(),
        }];

        let reply = manager
            .generate_reply(id, &ModelId::new("mock-model"), messages)
            .await
            .unwrap();

        assert_eq!(reply.role, types::ChatRole::Assistant);
        assert!(reply.content.contains("Hello"));
    }

    #[tokio::test]
    async fn test_generate_reply_provider_not_found() {
        let manager = ProviderManager::new();

        let messages = vec![ChatMessage {
            role: types::ChatRole::User,
            content: "Hello".to_string(),
        }];

        let result = manager
            .generate_reply(ProviderId::new("missing"), &ModelId::new("model"), messages)
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_generate_reply_stream() {
        let mut manager = ProviderManager::new();
        let id = ProviderId::new("mock");
        manager.register_provider(id.clone(), Box::new(MockProvider::new("mock")));

        let messages = vec![ChatMessage {
            role: types::ChatRole::User,
            content: "Hello".to_string(),
        }];

        let stream = manager
            .generate_reply_stream(id, &ModelId::new("mock-model"), messages)
            .await
            .unwrap();

        use futures::StreamExt;
        let chunks: Vec<_> = stream.collect().await;
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].as_ref().unwrap().contains("Streamed response"));
    }

    #[tokio::test]
    async fn test_load_config_unknown_provider_type() {
        let mut manager = ProviderManager::new();
        let helper = ProviderManagerHelper::default();

        let config = ProviderManagerConfig {
            providers: vec![ProviderConfig {
                id: ProviderId::new("test"),
                r#type: "unknown".to_string(),
                config: toml::Value::Table(toml::map::Map::new()),
            }],
            models: vec![],
        };

        let result = manager.load_config(config, helper).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_load_config_model_for_missing_provider() {
        let mut manager = ProviderManager::new();
        let mut helper = ProviderManagerHelper::default();
        helper.register_factory::<MockFactory>().unwrap();

        let config = ProviderManagerConfig {
            providers: vec![ProviderConfig {
                id: ProviderId::new("mock"),
                r#type: "mock".to_string(),
                config: toml::Value::Table(toml::map::Map::new()),
            }],
            models: vec![ModelConfig {
                provider_id: ProviderId::new("missing"),
                config: toml::Value::Table(toml::map::Map::new()),
            }],
        };

        let result = manager.load_config(config, helper).await;
        assert!(matches!(result, Err(_)));
    }

    #[tokio::test]
    async fn test_load_config_success() {
        let mut manager = ProviderManager::new();
        let mut helper = ProviderManagerHelper::default();
        helper.register_factory::<MockFactory>().unwrap();

        let config = ProviderManagerConfig {
            providers: vec![ProviderConfig {
                id: ProviderId::new("mock"),
                r#type: "mock".to_string(),
                config: toml::Value::Table(toml::map::Map::new()),
            }],
            models: vec![],
        };

        let result = manager.load_config(config, helper).await;
        if let Err(ref e) = result {
            eprintln!("Error: {:?}", e);
        }
        assert!(result.is_ok());
        assert!(manager.has_provider(&ProviderId::new("mock")));
    }

    #[test]
    fn test_provider_error_display() {
        let err = ProviderError::ProviderNotFound(ProviderId::new("test"));
        assert!(err.to_string().contains("test"));

        let err = ProviderError::UnknownProviderType("foo".to_string());
        assert!(err.to_string().contains("foo"));

        let err = ProviderError::ConfigParseError("parse error".to_string());
        assert!(err.to_string().contains("parse error"));
    }
}
