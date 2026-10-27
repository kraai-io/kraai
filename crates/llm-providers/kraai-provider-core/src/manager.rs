use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use color_eyre::Result;
use futures::{StreamExt, future::join_all, stream::BoxStream};
use kraai_types::{ModelId, ProviderId};

use crate::config::{ModelConfig, ProviderManagerConfig};
use crate::definition::ValidationError;
use crate::error::ProviderError;
use crate::provider::{Model, Provider, ProviderRequest};
use crate::registry::ProviderRegistry;
use crate::request_context::ProviderRequestContext;
use crate::stream::ProviderStreamEvent;

#[derive(Default, Clone)]
pub struct ProviderManager {
    providers: Arc<BTreeMap<ProviderId, Arc<dyn Provider>>>,
    pricing: crate::pricing::Pricing,
    pub(crate) cache_warming: crate::cache_warming::CacheWarming,
    cooldowns: Arc<std::sync::Mutex<HashMap<ProviderId, Arc<crate::http_retry::ProviderCooldown>>>>,
}

impl ProviderManager {
    const PROVIDER_INITIALIZATION_CONCURRENCY: usize = 8;

    pub fn is_subscription(&self, provider: &ProviderId) -> bool {
        self.pricing.is_subscription(provider)
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe_model_catalog(&self) -> tokio::sync::watch::Receiver<u64> {
        self.pricing.catalog.subscribe()
    }

    pub fn register_provider(&mut self, id: ProviderId, mut provider: Box<dyn Provider>) {
        provider.set_model_catalog(self.pricing.catalog.clone());
        Arc::make_mut(&mut self.providers).insert(id, Arc::from(provider));
        self.cache_warming = Default::default();
    }

    fn cooldown(&self, id: &ProviderId) -> Arc<crate::http_retry::ProviderCooldown> {
        self.cooldowns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(id.clone())
            .or_default()
            .clone()
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

    pub fn providers(&self) -> impl Iterator<Item = (&ProviderId, &Arc<dyn Provider>)> {
        self.providers.iter()
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
                let catalog = pricing.catalog.clone();
                let models = models_by_provider.remove(&provider_id).unwrap_or_default();
                tasks.spawn(async move {
                    let mut provider = registry.create_provider(
                        &provider_config.type_id,
                        provider_config.id.clone(),
                        provider_config.config,
                    )?;
                    provider.set_model_catalog(catalog);
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
        self.pricing.start().await;
        self.providers = providers;
        self.cache_warming = Default::default();
        Ok(())
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

    pub fn supports_native_compaction(
        &self,
        provider_id: &ProviderId,
        model_id: &ModelId,
    ) -> Result<bool> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| ProviderError::ProviderNotFound(provider_id.clone()))?;
        Ok(provider.supports_native_compaction(model_id))
    }

    pub async fn compact_stream(
        &self,
        provider_id: &ProviderId,
        model_id: &ModelId,
        request: ProviderRequest,
        context: ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| ProviderError::ProviderNotFound(provider_id.clone()))?;
        let pricing_model = provider.pricing_model_id(model_id).await?;
        let context = context.with_cooldown(self.cooldown(provider_id));
        let stream = provider.compact_stream(model_id, request, &context).await?;
        Ok(self
            .pricing
            .apply(provider_id, model_id, &pricing_model, stream)
            .await)
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
        let cache_observer = match self.cache_usage_observer(
            &provider_id,
            model_id,
            &request,
            &request_context,
        ) {
            Ok(observer) => observer,
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "Cache feedback unavailable; continuing with the conversation");
                None
            }
        };
        let pricing_model = provider.pricing_model_id(model_id).await?;
        let request_context = request_context.with_cooldown(self.cooldown(&provider_id));
        let stream = provider
            .generate_reply_stream(model_id, request, &request_context)
            .await?;
        let priced = self
            .pricing
            .apply(&provider_id, model_id, &pricing_model, stream)
            .await;
        Ok(priced
            .map(move |event| {
                if let (Some(observer), Ok(ProviderStreamEvent::Usage(usage))) =
                    (&cache_observer, &event)
                    && let Err(error) = observer.observe(usage)
                {
                    tracing::warn!(error = %error, "Unable to update cache warming feedback");
                }
                event
            })
            .boxed())
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
                    options: Vec::new(),
                    remove_options: Vec::new(),
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
}
