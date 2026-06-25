use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use color_eyre::Result;
use futures::{future::join_all, stream::BoxStream};
use kraai_types::{ChatMessage, ModelId, ProviderId};

use crate::config::{ModelConfig, ProviderManagerConfig};
use crate::definition::ValidationError;
use crate::error::{ProviderError, ProviderModelCacheRefreshError};
use crate::http_retry::ProviderRequestContext;
use crate::provider::{Model, Provider};
use crate::registry::ProviderRegistry;
use crate::stream::ProviderStreamEvent;

#[derive(Default, Clone)]
pub struct ProviderManager {
    providers: BTreeMap<ProviderId, Arc<dyn Provider>>,
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

    use crate::config::{DynamicConfig, DynamicValue, ProviderConfig};
    use crate::test_support::{MockFactory, MockProvider, simple_provider_definition};

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
            simple_provider_definition(
                "Observed",
                "Records initialization concurrency",
                false,
                "observed",
            ),
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
            simple_provider_definition("Failing", "Fails during creation", false, "failing"),
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
            simple_provider_definition(
                "Failing Cache",
                "Fails while refreshing its model cache",
                true,
                "failing-cache",
            ),
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
