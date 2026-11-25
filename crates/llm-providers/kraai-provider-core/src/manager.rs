use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use color_eyre::Result;
use futures::{future::join_all, stream::BoxStream};
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
        let context = context.with_cooldown(self.cooldown(provider_id));
        let stream = provider.compact_stream(model_id, request, &context).await?;
        Ok(self.pricing.apply(provider_id, model_id, stream).await)
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
        let request_context = request_context.with_cooldown(self.cooldown(&provider_id));
        let stream = provider
            .generate_reply_stream(model_id, request, &request_context)
            .await?;
        Ok(self.pricing.apply(&provider_id, model_id, stream).await)
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
mod tests;
