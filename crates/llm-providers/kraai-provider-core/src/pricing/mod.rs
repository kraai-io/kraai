mod catalog;
mod config;

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::{StreamExt, stream::BoxStream};
use kraai_types::{ModelId, ProviderId, RequestCost};

use crate::{ProviderManagerConfig, ProviderStreamEvent};
use catalog::Catalog;
use config::PricingConfig;

pub use config::{pricing_fields, validate_pricing_config};

#[derive(Clone, Default)]
pub struct Pricing {
    configs: BTreeMap<ProviderId, PricingConfig>,
    catalog: Arc<Catalog>,
}

impl Pricing {
    pub fn new(config: &ProviderManagerConfig) -> color_eyre::Result<Self> {
        let configs = config
            .providers
            .iter()
            .map(|provider| {
                Ok((
                    provider.id.clone(),
                    PricingConfig::new(provider, &config.models)?,
                ))
            })
            .collect::<color_eyre::Result<_>>()?;
        Ok(Self {
            configs,
            catalog: Arc::new(Catalog::default()),
        })
    }

    pub async fn start(&self) {
        if !self.configs.values().any(|config| {
            !config.subscription && (config.api.is_some() || config.provider.is_some())
        }) {
            return;
        }
        self.catalog.load().await;
        let catalog = Arc::downgrade(&self.catalog);
        tokio::spawn(async move {
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

    pub fn is_subscription(&self, provider: &ProviderId) -> bool {
        self.configs
            .get(provider)
            .is_some_and(|config| config.subscription)
    }

    pub async fn apply(
        &self,
        provider: &ProviderId,
        model: &ModelId,
        stream: BoxStream<'static, color_eyre::Result<ProviderStreamEvent>>,
    ) -> BoxStream<'static, color_eyre::Result<ProviderStreamEvent>> {
        let Some(config) = self
            .configs
            .get(provider)
            .filter(|config| !config.subscription)
        else {
            return stream;
        };
        let configured = config.models.get(model).cloned();
        let catalog_pricing = if configured.is_none() {
            self.catalog
                .lookup(
                    config.provider.as_deref(),
                    config.api.as_deref(),
                    model.as_str(),
                )
                .await
        } else {
            None
        };
        stream
            .map(move |event| match event {
                Ok(ProviderStreamEvent::Usage(mut usage)) => {
                    if usage.cost.is_none() {
                        let pricing = match &configured {
                            Some(rates) => Some((rates.clone(), String::from("configured"), now())),
                            None => {
                                catalog_pricing
                                    .as_ref()
                                    .and_then(|(cost, source, timestamp)| {
                                        catalog::parse_rates(cost, &usage)
                                            .map(|rates| (rates, source.clone(), *timestamp))
                                    })
                            }
                        };
                        if let Some((rates, source, priced_at)) = pricing
                            && let Some(amount) = rates.estimate(&usage)
                        {
                            usage.cost = Some(RequestCost {
                                amount,
                                source,
                                rates: Some(rates),
                                priced_at,
                                upstream: None,
                            });
                        }
                    }
                    Ok(ProviderStreamEvent::Usage(usage))
                }
                other => other,
            })
            .boxed()
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "tests combine fallible setup with assertions"
)]
mod tests {
    use super::*;
    use crate::{DynamicConfig, DynamicValue, ModelConfig, ProviderConfig};
    use kraai_types::{TokenUsage, Usd};

    #[tokio::test]
    async fn configured_rates_estimate_usage_but_never_replace_a_reported_zero()
    -> color_eyre::Result<()> {
        let provider = ProviderId::new("custom");
        let model = ModelId::new("model");
        let pricing = Pricing::new(&ProviderManagerConfig {
            providers: vec![ProviderConfig {
                id: provider.clone(),
                type_id: "openai-chat-completions".into(),
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
        })?;
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
        let mut stream = pricing.apply(&provider, &model, source).await;
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
