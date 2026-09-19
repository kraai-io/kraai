mod catalog;
mod config;
mod policy;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use futures::{StreamExt, stream::BoxStream};
use kraai_types::{ModelId, ProviderId, RequestCost, TokenRates, TokenUsage};
use tokio_util::task::AbortOnDropHandle;

use crate::{ProviderManagerConfig, ProviderStreamEvent};
use catalog::Catalog;
use config::PricingConfig;

pub use config::{pricing_fields, validate_pricing_config};
pub use policy::{ProviderPricingCatalog, ProviderPricingPolicy};

#[derive(Clone, Default)]
pub struct Pricing {
    configs: Arc<BTreeMap<ProviderId, PricingConfig>>,
    catalog: Arc<Catalog>,
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

    pub async fn refresh(&self) {
        if !self.uses_catalog() {
            return;
        }
        self.catalog.load().await;
        self.catalog.refresh().await;
    }

    pub async fn start(&self) {
        if !self.uses_catalog() {
            return;
        }
        self.refresh().await;
        let catalog = Arc::downgrade(&self.catalog);
        self.start_refresh_task(async move {
            loop {
                let Some(current) = catalog.upgrade() else {
                    break;
                };
                current.refresh().await;
                drop(current);
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
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

    pub async fn quote(
        &self,
        provider: &ProviderId,
        model: &ModelId,
        pricing_model: &ModelId,
    ) -> Option<PriceQuote> {
        let config = self.configs.get(provider)?;
        let configured = config
            .models
            .get(model)
            .or_else(|| config.models.get(pricing_model))
            .cloned();
        let catalog_pricing = if configured.is_none() {
            self.catalog
                .lookup(
                    config.provider.as_deref(),
                    config.api.as_deref(),
                    pricing_model.as_str(),
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
        pricing_model: &ModelId,
        stream: BoxStream<'static, color_eyre::Result<ProviderStreamEvent>>,
    ) -> BoxStream<'static, color_eyre::Result<ProviderStreamEvent>> {
        let quote = self.quote(provider, model, pricing_model).await;
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
            } => (
                catalog::parse_rates(cost, usage)?,
                source.clone(),
                *timestamp,
            ),
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
mod tests {
    use color_eyre::eyre::{ensure, eyre};

    use super::*;
    use crate::{DynamicConfig, DynamicValue, ModelConfig, ProviderConfig};
    use kraai_types::{TokenUsage, Usd};
    use tokio_util::sync::CancellationToken;

    #[test]
    fn model_index_preserves_provider_order_and_duplicate_rate_precedence() -> color_eyre::Result<()>
    {
        let provider = |id: &str, kind: &str| ProviderConfig {
            id: ProviderId::new(id),
            type_id: kind.to_owned(),
            config: DynamicConfig::new(),
        };
        let model = |provider: &str, input: Option<&str>| ModelConfig {
            id: ModelId::new("model"),
            provider_id: ProviderId::new(provider),
            config: input.map_or_else(DynamicConfig::new, |input| {
                DynamicConfig::from([
                    (String::from("price_input"), DynamicValue::from(input)),
                    (String::from("price_output"), DynamicValue::from("8")),
                ])
            }),
        };
        let config = ProviderManagerConfig {
            providers: vec![
                provider("b", "first"),
                provider("a", "second"),
                provider("b", "last"),
                provider("empty", "empty"),
            ],
            models: vec![
                model("a", Some("2")),
                model("b", Some("3")),
                model("b", None),
                model("orphan", Some("invalid")),
                model("a", Some("4")),
            ],
        };
        for providers in [
            config.providers.clone(),
            vec![provider("b", "single")],
            Vec::new(),
        ] {
            let current = ProviderManagerConfig {
                providers,
                models: config.models.clone(),
            };
            let calls = std::cell::RefCell::new(Vec::new());
            let actual = Pricing::new(&current, |kind| {
                calls.borrow_mut().push(kind.to_owned());
                ProviderPricingPolicy {
                    subscription: kind == "last",
                    ..ProviderPricingPolicy::default()
                }
            })?;
            ensure!(
                *calls.borrow()
                    == current
                        .providers
                        .iter()
                        .map(|provider| provider.type_id.clone())
                        .collect::<Vec<_>>()
            );
            let expected = current
                .providers
                .iter()
                .map(|provider| {
                    Ok((
                        provider.id.clone(),
                        PricingConfig::new(
                            provider,
                            &current.models,
                            ProviderPricingPolicy {
                                subscription: provider.type_id == "last",
                                ..ProviderPricingPolicy::default()
                            },
                        )?,
                    ))
                })
                .collect::<color_eyre::Result<BTreeMap<_, _>>>()?;
            ensure!(actual.configs.len() == expected.len());
            for (id, expected) in expected {
                let actual = actual
                    .configs
                    .get(&id)
                    .ok_or_else(|| eyre!("missing provider {id}"))?;
                ensure!(actual.models == expected.models);
                ensure!(actual.subscription == expected.subscription);
                ensure!(actual.api == expected.api && actual.provider == expected.provider);
            }
        }
        Ok(())
    }

    #[test]
    fn model_index_preserves_first_validation_error_and_policy_callback_order()
    -> color_eyre::Result<()> {
        let config = ProviderManagerConfig {
            providers: ["b", "a"]
                .map(|id| ProviderConfig {
                    id: ProviderId::new(id),
                    type_id: id.to_owned(),
                    config: DynamicConfig::new(),
                })
                .to_vec(),
            models: [("a", "bad", "1"), ("b", "1", "bad"), ("b", "bad", "1")]
                .map(|(provider, input, output)| ModelConfig {
                    id: ModelId::new("model"),
                    provider_id: ProviderId::new(provider),
                    config: DynamicConfig::from([
                        (String::from("price_input"), DynamicValue::from(input)),
                        (String::from("price_output"), DynamicValue::from(output)),
                    ]),
                })
                .to_vec(),
        };
        let calls = std::cell::RefCell::new(Vec::new());
        let error = Pricing::new(&config, |kind| {
            calls.borrow_mut().push(kind.to_owned());
            ProviderPricingPolicy::default()
        })
        .err()
        .ok_or_else(|| eyre!("invalid rates passed validation"))?;
        ensure!(
            error.to_string() == "price_output must be a non-negative USD price per million tokens"
        );
        ensure!(*calls.borrow() == ["b"]);
        Ok(())
    }

    #[tokio::test]
    async fn refresh_task_is_shared_and_stops_with_the_last_pricing_owner() -> color_eyre::Result<()>
    {
        let pricing = Pricing::default();
        let clone = pricing.clone();
        let started = CancellationToken::new();
        let stopped = CancellationToken::new();
        let started_for_task = started.clone();
        let stop_guard = stopped.clone().drop_guard();
        let (check_tx, check_rx) = tokio::sync::oneshot::channel();
        let (alive_tx, alive_rx) = tokio::sync::oneshot::channel();
        pricing.start_refresh_task(async move {
            let _stop_guard = stop_guard;
            started_for_task.cancel();
            if check_rx.await.is_ok() {
                let _ = alive_tx.send(());
            }
            std::future::pending().await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), started.cancelled()).await?;

        let duplicate_dropped = CancellationToken::new();
        let duplicate_guard = duplicate_dropped.clone().drop_guard();
        clone.start_refresh_task(async move {
            let _duplicate_guard = duplicate_guard;
            std::future::pending().await
        });
        color_eyre::eyre::ensure!(
            duplicate_dropped.is_cancelled(),
            "duplicate background refresh worker was started"
        );

        drop(pricing);
        if check_tx.send(()).is_err() {
            return Err(color_eyre::eyre::eyre!(
                "refresh worker stopped while another pricing owner remained"
            ));
        }
        tokio::time::timeout(std::time::Duration::from_secs(1), alive_rx).await??;
        color_eyre::eyre::ensure!(
            !stopped.is_cancelled(),
            "refresh worker stopped while another pricing owner remained"
        );
        drop(clone);
        tokio::time::timeout(std::time::Duration::from_secs(1), stopped.cancelled()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn configured_rates_estimate_usage_but_never_replace_a_reported_zero()
    -> color_eyre::Result<()> {
        check_configured_rates(false).await?;
        check_configured_rates(true).await
    }

    async fn check_configured_rates(subscription: bool) -> color_eyre::Result<()> {
        let provider = ProviderId::new("custom");
        let model = ModelId::new("model");
        let pricing = Pricing::new(
            &ProviderManagerConfig {
                providers: vec![ProviderConfig {
                    id: provider.clone(),
                    type_id: "custom-factory".into(),
                    config: DynamicConfig::new(),
                }],
                models: vec![ModelConfig {
                    id: model.clone(),
                    provider_id: provider.clone(),
                    config: DynamicConfig::from([
                        ("price_input".into(), DynamicValue::from("2")),
                        ("price_output".into(), DynamicValue::from("8")),
                    ]),
                }],
            },
            |_| ProviderPricingPolicy {
                subscription,
                ..ProviderPricingPolicy::default()
            },
        )?;
        assert_eq!(pricing.is_subscription(&provider), subscription);
        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 10,
            ..Default::default()
        };
        let reported = TokenUsage {
            cost: Some(RequestCost {
                amount: Usd(0),
                source: "openrouter".into(),
                rates: None,
                priced_at: 1,
                upstream: None,
            }),
            ..usage.clone()
        };
        let source = futures::stream::iter(vec![
            Ok(ProviderStreamEvent::Usage(usage)),
            Ok(ProviderStreamEvent::Usage(reported)),
        ])
        .boxed();
        let mut stream = pricing.apply(&provider, &model, &model, source).await;
        let Some(Ok(ProviderStreamEvent::Usage(estimated))) = stream.next().await else {
            return Err(color_eyre::eyre::eyre!("missing estimated usage"));
        };
        assert!(
            estimated
                .cost
                .is_some_and(|cost| cost.amount == Usd(440_000) && cost.rates.is_some())
        );
        let Some(Ok(ProviderStreamEvent::Usage(reported))) = stream.next().await else {
            return Err(color_eyre::eyre::eyre!("missing reported usage"));
        };
        assert!(
            reported
                .cost
                .is_some_and(|cost| cost.amount == Usd(0) && cost.rates.is_none())
        );
        Ok(())
    }
}
