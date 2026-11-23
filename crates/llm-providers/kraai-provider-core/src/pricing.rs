mod config;
mod policy;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{StreamExt, stream::BoxStream};
use kraai_types::{ModelId, ProviderId, RequestCost, TokenRates, TokenUsage};
use tokio_util::task::AbortOnDropHandle;

use crate::model_catalog::{ModelCatalog as Catalog, parse_rates};
use crate::{ProviderManagerConfig, ProviderStreamEvent};
use config::PricingConfig;

pub use config::{pricing_fields, validate_pricing_config};
pub use policy::{ProviderPricingCatalog, ProviderPricingPolicy};

const REFRESH_INTERVAL: Duration = Duration::from_secs(3600);
const RETRY_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub struct Pricing {
    configs: Arc<BTreeMap<ProviderId, PricingConfig>>,
    pub(crate) catalog: Arc<Catalog>,
    refresh_task: Arc<Mutex<Option<AbortOnDropHandle<()>>>>,
}

impl Pricing {
    pub fn new(
        config: &ProviderManagerConfig,
        policy_for: impl Fn(&str) -> ProviderPricingPolicy,
    ) -> color_eyre::Result<Self> {
        let indexed_models = (config.providers.len() > 1).then(|| {
            let mut grouped: BTreeMap<_, Vec<_>> = BTreeMap::new();
            for model in &config.models {
                grouped.entry(&model.provider_id).or_default().push(model);
            }
            grouped
        });
        let configs = config
            .providers
            .iter()
            .map(|provider| {
                let policy = policy_for(&provider.type_id);
                let pricing = match &indexed_models {
                    Some(models) => PricingConfig::new(
                        provider,
                        models.get(&provider.id).into_iter().flatten().copied(),
                        policy,
                    ),
                    None => PricingConfig::new(provider, &config.models, policy),
                }?;
                Ok((provider.id.clone(), pricing))
            })
            .collect::<color_eyre::Result<_>>()?;
        Ok(Self {
            configs: Arc::new(configs),
            catalog: Arc::new(Catalog::default()),
            refresh_task: Arc::default(),
        })
    }

    fn uses_catalog(&self) -> bool {
        self.configs
            .values()
            .any(|config| config.api.is_some() || config.provider.is_some())
    }

    pub async fn start(&self) {
        if !self.uses_catalog() {
            return;
        }
        self.catalog.initialize().await;
        let catalog = Arc::downgrade(&self.catalog);
        self.start_refresh_task(async move {
            loop {
                let Some(current) = catalog.upgrade() else {
                    break;
                };
                let fresh = current.refresh().await;
                drop(current);
                let interval = if fresh {
                    REFRESH_INTERVAL
                } else {
                    RETRY_INTERVAL
                };
                tokio::time::sleep(interval).await;
            }
        });
    }

    fn start_refresh_task(&self, worker: impl Future<Output = ()> + Send + 'static) {
        let mut task = self
            .refresh_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if task.as_ref().is_none_or(AbortOnDropHandle::is_finished) {
            *task = Some(AbortOnDropHandle::new(tokio::spawn(worker)));
        }
    }

    pub fn is_subscription(&self, provider: &ProviderId) -> bool {
        self.configs
            .get(provider)
            .is_some_and(|config| config.subscription)
    }

    pub async fn quote(&self, provider: &ProviderId, model: &ModelId) -> Option<PriceQuote> {
        let config = self.configs.get(provider)?;
        let configured = config.models.get(model).cloned();
        let catalog_pricing = if configured.is_none() {
            self.catalog
                .lookup(
                    config.provider.as_deref(),
                    config.api.as_deref(),
                    model.as_str(),
                    config.source,
                )
                .await
        } else {
            None
        };
        match configured {
            Some(rates) => Some(PriceQuote::Configured(rates)),
            None => catalog_pricing.map(|(cost, source, timestamp)| PriceQuote::Catalog {
                cost,
                source,
                timestamp,
            }),
        }
    }

    pub async fn apply(
        &self,
        provider: &ProviderId,
        model: &ModelId,
        stream: BoxStream<'static, color_eyre::Result<ProviderStreamEvent>>,
    ) -> BoxStream<'static, color_eyre::Result<ProviderStreamEvent>> {
        let quote = self.quote(provider, model).await;
        stream
            .map(move |event| match event {
                Ok(ProviderStreamEvent::Usage(mut usage)) => {
                    if usage.cost.is_none() {
                        usage.cost = quote.as_ref().and_then(|quote| quote.estimate(&usage));
                    }
                    Ok(ProviderStreamEvent::Usage(usage))
                }
                other => other,
            })
            .boxed()
    }
}

#[derive(Clone)]
pub enum PriceQuote {
    Configured(TokenRates),
    Catalog {
        cost: serde_json::Value,
        source: String,
        timestamp: u64,
    },
}

impl PriceQuote {
    pub fn estimate(&self, usage: &TokenUsage) -> Option<RequestCost> {
        let (rates, source, priced_at) = match self {
            Self::Configured(rates) => (rates.clone(), String::from("configured"), now()),
            Self::Catalog {
                cost,
                source,
                timestamp,
            } => (parse_rates(cost, usage)?, source.clone(), *timestamp),
        };
        Some(RequestCost {
            amount: rates.estimate(usage)?,
            source,
            rates: Some(rates),
            priced_at,
            upstream: None,
        })
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests;
