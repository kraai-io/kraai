use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use color_eyre::Result;
use futures::{future::join_all, stream::BoxStream};
use kraai_types::{ModelId, ProviderId};

use crate::config::{ModelConfig, ProviderManagerConfig};
use crate::definition::ValidationError;
use crate::error::{ProviderError, ProviderModelCacheRefreshError};
use crate::provider::{Model, Provider, ProviderRequest, ScriptToolTransport};
use crate::registry::ProviderRegistry;
use crate::request_context::ProviderRequestContext;
use crate::stream::ProviderStreamEvent;

#[derive(Default, Clone)]
pub struct ProviderManager {
    providers: Arc<BTreeMap<ProviderId, Arc<dyn Provider>>>,
    pricing: crate::pricing::Pricing,
}

impl ProviderManager {
    const PROVIDER_INITIALIZATION_CONCURRENCY: usize = 8;
    const MODEL_CACHE_REFRESH_CONCURRENCY: usize = 8;

    pub fn is_subscription(&self, provider: &ProviderId) -> bool {
        self.pricing.is_subscription(provider)
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_provider(&mut self, id: ProviderId, provider: Box<dyn Provider>) {
        Arc::make_mut(&mut self.providers).insert(id, Arc::from(provider));
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
        let pricing = crate::pricing::Pricing::new(&config, |type_id| {
            registry.pricing_policy(type_id).unwrap_or_default()
        })?;
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

            provider_configs.insert(provider_config.id.clone(), provider_config);
        }

        for model_config in config.models {
            let provider_config =
                provider_configs
                    .get(&model_config.provider_id)
                    .ok_or_else(|| {
                        ProviderError::ProviderNotRegistered(model_config.provider_id.clone())
                    })?;
            let errors =
                registry.validate_model_config(&provider_config.type_id, &model_config.config)?;
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

        let providers: BTreeMap<ProviderId, Arc<dyn Provider>> = providers
            .into_iter()
            .map(|(id, provider)| (id, Arc::from(provider)))
            .collect();
        let providers = Arc::new(providers);

        self.pricing = pricing;
        let (_, refresh_result) = tokio::join!(
            async {
                self.pricing.start().await;
                self.providers = providers.clone();
            },
            Self::update_models_list_for(&providers),
        );
        refresh_result
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
        let mut providers = providers.iter();
        let mut tasks = tokio::task::JoinSet::new();
        let mut failures = Vec::new();
        loop {
            while tasks.len() < Self::MODEL_CACHE_REFRESH_CONCURRENCY {
                let Some((provider_id, provider)) = providers.next() else {
                    break;
                };
                let provider_id = provider_id.clone();
                let provider = provider.clone();
                tasks.spawn(async move {
                    provider
                        .cache_models()
                        .await
                        .map_err(|error| ProviderModelCacheRefreshError {
                            provider_id,
                            message: error.to_string(),
                        })
                });
            }

            let Some(result) = tasks.join_next().await else {
                break;
            };
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

    pub fn script_tool_transport(
        &self,
        provider_id: &ProviderId,
        model_id: &ModelId,
    ) -> Result<ScriptToolTransport> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| ProviderError::ProviderNotFound(provider_id.clone()))?;
        Ok(provider.script_tool_transport(model_id))
    }

    pub async fn generate_reply_stream(
        &self,
        provider_id: ProviderId,
        model_id: &ModelId,
        request: ProviderRequest,
        request_context: ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        let provider = self
            .providers
            .get(&provider_id)
            .ok_or_else(|| ProviderError::ProviderNotFound(provider_id.clone()))?;
        let pricing_model = provider.pricing_model_id(model_id).await?;
        let stream = provider
            .generate_reply_stream(model_id, request, &request_context)
            .await?;
        Ok(self
            .pricing
            .apply(&provider_id, model_id, &pricing_model, stream)
            .await)
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
    use std::sync::atomic::{AtomicUsize, Ordering};
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
        manager.register_provider(
            ProviderId::new("active"),
            Box::new(MockProvider::new("active")),
        );
        let snapshot = manager.clone();
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

        assert_eq!(manager.list_providers(), vec![ProviderId::new("mock")]);
        assert_eq!(snapshot.list_providers(), vec![ProviderId::new("active")]);
        Ok(())
    }

    #[test]
    fn cloned_managers_isolate_provider_registration() -> Result<()> {
        let id = ProviderId::new("shared");
        let mut manager = ProviderManager::new();
        manager.register_provider(id.clone(), Box::new(MockProvider::new("original")));
        let mut snapshot = manager.clone();
        let original = snapshot
            .get_provider(&id)
            .ok_or_else(|| eyre!("snapshot lost its provider"))?;
        assert!(Arc::ptr_eq(
            &original,
            &manager
                .get_provider(&id)
                .ok_or_else(|| eyre!("manager lost its provider"))?
        ));

        manager.register_provider(id.clone(), Box::new(MockProvider::new("replacement")));
        snapshot.register_provider(
            ProviderId::new("snapshot-only"),
            Box::new(MockProvider::new("snapshot-only")),
        );
        assert_eq!(manager.list_providers(), vec![id.clone()]);
        assert_eq!(
            snapshot.list_providers(),
            vec![id.clone(), ProviderId::new("snapshot-only")]
        );
        assert_eq!(
            manager
                .get_provider(&id)
                .ok_or_else(|| eyre!("manager lost replacement provider"))?
                .get_provider_id(),
            ProviderId::new("replacement")
        );
        assert!(Arc::ptr_eq(
            &original,
            &snapshot
                .get_provider(&id)
                .ok_or_else(|| eyre!("snapshot lost original provider"))?
        ));
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

            async fn generate_reply_stream(
                &self,
                _model_id: &ModelId,
                _request: ProviderRequest,
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
            crate::ProviderPricingPolicy::default(),
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

    #[test]
    fn model_discovery_overlaps_pricing_without_publishing_before_pricing_finishes() -> Result<()> {
        struct ObservedProvider {
            id: ProviderId,
            cache_started: Arc<tokio::sync::Notify>,
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
                self.cache_started.notify_one();
                Ok(())
            }

            async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
                Ok(())
            }

            async fn generate_reply_stream(
                &self,
                _model_id: &ModelId,
                _request: ProviderRequest,
                _request_context: &ProviderRequestContext,
            ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
                unreachable!("not used by this test")
            }
        }

        if directories::BaseDirs::new().is_none() {
            return Err(eyre!("test requires a pricing cache directory"));
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()?
            .block_on(async {
                let cache_started = Arc::new(tokio::sync::Notify::new());
                let mut registry = ProviderRegistry::default();
                let started = cache_started.clone();
                registry.register_dynamic_factory(
                    "observed",
                    simple_provider_definition("Observed", "Observed", true, "observed"),
                    crate::ProviderPricingPolicy::default(),
                    move |id, _config| {
                        Ok(Box::new(ObservedProvider {
                            id,
                            cache_started: started.clone(),
                        }))
                    },
                    |_| Vec::new(),
                    |_| Vec::new(),
                )?;
                let config = ProviderManagerConfig {
                    providers: vec![ProviderConfig {
                        id: ProviderId::new("replacement"),
                        type_id: String::from("observed"),
                        config: DynamicConfig::from([(
                            String::from("base_url"),
                            DynamicValue::from("http://127.0.0.1"),
                        )]),
                    }],
                    models: Vec::new(),
                };
                let mut manager = ProviderManager::new();
                manager.register_provider(
                    ProviderId::new("active"),
                    Box::new(MockProvider::new("active")),
                );
                let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
                let (blocked_tx, blocked_rx) = tokio::sync::oneshot::channel();
                let blocker = tokio::task::spawn_blocking(move || {
                    let _ = blocked_tx.send(());
                    let _ = release_rx.recv();
                });
                blocked_rx.await?;

                let mut loading = Box::pin(manager.load_config(config, registry));
                let observed = tokio::select! {
                    result = &mut loading => Err(eyre!(
                        "startup completed before pricing I/O was released: {result:?}"
                    )),
                    result = tokio::time::timeout(
                        Duration::from_secs(2),
                        cache_started.notified(),
                    ) => result.map_err(|error| eyre!("model discovery did not start: {error}")),
                };
                let still_pending = observed.is_ok() && futures::poll!(&mut loading).is_pending();
                drop(loading);
                drop(release_tx);
                blocker.await?;
                observed?;
                assert!(still_pending);
                assert_eq!(manager.list_providers(), vec![ProviderId::new("active")]);
                Ok(())
            })
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
            crate::ProviderPricingPolicy::default(),
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
        let snapshot = manager.clone();

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
        assert_eq!(snapshot.list_providers(), vec![ProviderId::new("active")]);
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
            crate::ProviderPricingPolicy::default(),
            |id, _config| Ok(Box::new(MockProvider::failing_cache(id.as_str()))),
            |_| Vec::new(),
            |_| Vec::new(),
        )?;

        let mut manager = ProviderManager::new();
        manager.register_provider(
            ProviderId::new("active"),
            Box::new(MockProvider::new("active")),
        );
        let snapshot = manager.clone();

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
        assert_eq!(snapshot.list_providers(), vec![ProviderId::new("active")]);
        Ok(())
    }

    #[tokio::test]
    async fn update_models_list_attempts_every_provider_before_returning_failures() -> Result<()> {
        let failing = Arc::new(MockProvider::failing_cache("failing"));
        let successful = Arc::new(MockProvider::new("successful"));

        let mut manager = ProviderManager::new();
        Arc::make_mut(&mut manager.providers).insert(ProviderId::new("failing"), failing.clone());
        Arc::make_mut(&mut manager.providers)
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
    async fn update_models_list_bounds_refreshes_and_attempts_later_providers_after_failure()
    -> Result<()> {
        struct ObservedProvider {
            id: ProviderId,
            active: Arc<AtomicUsize>,
            peak: Arc<AtomicUsize>,
            completed: Arc<AtomicUsize>,
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
                self.peak.fetch_max(active, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(25)).await;
                self.active.fetch_sub(1, Ordering::SeqCst);
                self.completed.fetch_add(1, Ordering::SeqCst);
                if self.id.as_str() == "provider-00" {
                    Err(eyre!("first provider failed"))
                } else {
                    Ok(())
                }
            }

            async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
                Ok(())
            }

            async fn generate_reply_stream(
                &self,
                _model_id: &ModelId,
                _request: ProviderRequest,
                _request_context: &ProviderRequestContext,
            ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
                unreachable!("not used by this test")
            }
        }

        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let mut manager = ProviderManager::new();
        let provider_count = ProviderManager::MODEL_CACHE_REFRESH_CONCURRENCY + 3;
        for index in 0..provider_count {
            let id = ProviderId::new(format!("provider-{index:02}"));
            Arc::make_mut(&mut manager.providers).insert(
                id.clone(),
                Arc::new(ObservedProvider {
                    id,
                    active: active.clone(),
                    peak: peak.clone(),
                    completed: completed.clone(),
                }),
            );
        }

        let Err(error) = manager.update_models_list().await else {
            return Err(eyre!("first provider unexpectedly succeeded"));
        };

        assert_eq!(completed.load(Ordering::SeqCst), provider_count);
        assert_eq!(
            peak.load(Ordering::SeqCst),
            ProviderManager::MODEL_CACHE_REFRESH_CONCURRENCY,
        );
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::ModelCacheRefreshFailed(failures))
                if failures == &vec![ProviderModelCacheRefreshError {
                    provider_id: ProviderId::new("provider-00"),
                    message: String::from("first provider failed"),
                }]
        ));
        Ok(())
    }
}
