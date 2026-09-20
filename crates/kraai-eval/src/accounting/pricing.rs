use std::path::PathBuf;
use std::sync::Arc;

use color_eyre::eyre::{Result, ensure};
use kraai_provider_core::{
    DynamicConfig, DynamicValue, Pricing, ProviderConfig, ProviderFactory, ProviderManagerConfig,
    ProviderPricingPolicy,
};
use kraai_provider_openai_chat_completions::{OpenAiChatCompletionsFactory, OpenAiFactory};
use kraai_provider_openai_codex::OpenAiCodexFactory;
use kraai_types::{ModelId, ProviderId, RequestCost, TokenUsage};

use super::RequestMeasurement;

#[derive(Debug, Clone, Default)]
pub struct PricingOptions {
    pub config: Option<PathBuf>,
    pub provider: Option<String>,
    pub(crate) snapshot: Option<Arc<ProviderManagerConfig>>,
}

impl PricingOptions {
    pub fn new(config: Option<PathBuf>, provider: Option<String>) -> Self {
        Self {
            config,
            provider,
            snapshot: None,
        }
    }

    pub fn freeze(&self) -> Result<Self> {
        let snapshot = RequestPricing::configuration(self)?;
        Pricing::new(&snapshot, pricing_policy)?;
        Ok(Self {
            snapshot: Some(snapshot),
            ..self.clone()
        })
    }

    pub fn validate(&self) -> Result<()> {
        let config = RequestPricing::configuration(self)?;
        Pricing::new(&config, pricing_policy)?;
        Ok(())
    }
}

pub(super) struct RequestPricing {
    pricing: Pricing,
    provider: ProviderId,
}

impl RequestPricing {
    fn configuration(options: &PricingOptions) -> Result<Arc<ProviderManagerConfig>> {
        if let Some(snapshot) = &options.snapshot {
            return Ok(snapshot.clone());
        }
        let config = if let Some(path) = &options.config {
            let mut config: ProviderManagerConfig = toml::from_slice(&std::fs::read(path)?)?;
            config.providers.retain(|provider| {
                options
                    .provider
                    .as_ref()
                    .is_none_or(|id| provider.id.as_str() == id)
            });
            ensure!(
                config.providers.len() == 1,
                "pricing config must select exactly one provider; use --pricing-provider"
            );
            config
        } else {
            ensure!(
                options.provider.is_none(),
                "pricing-provider requires pricing-config"
            );
            ProviderManagerConfig {
                providers: vec![ProviderConfig {
                    id: ProviderId::new("benchmark"),
                    type_id: String::from("openai"),
                    config: DynamicConfig::from([(
                        String::from("pricing_provider"),
                        DynamicValue::from("openai"),
                    )]),
                }],
                models: vec![],
            }
        };
        Ok(Arc::new(config))
    }

    pub async fn load(options: &PricingOptions) -> Result<Self> {
        let config = Self::configuration(options)?;
        let provider = config
            .providers
            .first()
            .ok_or_else(|| color_eyre::eyre::eyre!("missing pricing provider"))?
            .id
            .clone();
        let pricing = Pricing::new(&config, pricing_policy)?;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), pricing.refresh()).await;
        Ok(Self { pricing, provider })
    }

    pub async fn estimate(
        &self,
        request: &RequestMeasurement,
    ) -> std::result::Result<(RequestCost, String), String> {
        let usage = request
            .usage
            .as_ref()
            .ok_or_else(|| String::from("missing request usage"))?;
        let model = request
            .model
            .as_deref()
            .ok_or_else(|| String::from("missing actual model"))?;
        if request
            .service_tier
            .as_deref()
            .is_some_and(|tier| !matches!(tier, "auto" | "default"))
        {
            return Err(String::from("service tier pricing is unavailable"));
        }
        let model = ModelId::new(model);
        let quote = self
            .pricing
            .quote(&self.provider, &model, &model)
            .await
            .ok_or_else(|| String::from("model pricing is unavailable"))?;
        let basis = match &quote {
            kraai_provider_core::PriceQuote::Configured(rates) => {
                serde_json::json!({"model": model.as_str(), "configured": rates})
            }
            kraai_provider_core::PriceQuote::Catalog { cost, source, .. } => {
                serde_json::json!({"model": model.as_str(), "catalog": cost, "source": source})
            }
        };
        let basis = hash_pricing_basis(basis).map_err(|error| error.to_string())?;
        let convert = |value: u64| {
            usize::try_from(value).map_err(|error| format!("token count overflow: {error}"))
        };
        let usage = TokenUsage {
            input_tokens: convert(usage.input_tokens)?,
            output_tokens: convert(usage.output_tokens)?,
            cache_read_tokens: convert(usage.cache_read_tokens)?,
            cache_write_tokens: convert(usage.cache_write_tokens)?,
            reasoning_tokens: convert(usage.reasoning_tokens)?,
            total_tokens: convert(usage.total_tokens)?,
            cost: None,
        };
        quote
            .estimate(&usage)
            .map(|cost| (cost, basis))
            .ok_or_else(|| String::from("rates are incomplete or cost overflowed"))
    }
}

fn pricing_policy(type_id: &str) -> ProviderPricingPolicy {
    match type_id {
        OpenAiFactory::TYPE_ID => OpenAiFactory::pricing_policy(),
        OpenAiCodexFactory::TYPE_ID => OpenAiCodexFactory::pricing_policy(),
        _ => OpenAiChatCompletionsFactory::pricing_policy(),
    }
}

fn hash_pricing_basis(mut basis: serde_json::Value) -> serde_json::Result<String> {
    basis.sort_all_objects();
    Ok(crate::cache::hash_chunks(&[serde_json::to_vec(&basis)?]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pricing_policies_preserve_defaults_and_unknown_provider_endpoints() {
        let empty = DynamicConfig::new();
        let direct = pricing_policy(OpenAiFactory::TYPE_ID);
        assert_eq!((direct.catalog)(&empty).provider.as_deref(), Some("openai"));
        assert_eq!(
            (direct.catalog)(&empty).api.as_deref(),
            Some("https://api.openai.com/v1")
        );
        let subscription = pricing_policy(OpenAiCodexFactory::TYPE_ID);
        assert!(subscription.subscription);
        assert_eq!(
            (subscription.catalog)(&empty).provider.as_deref(),
            Some("openai")
        );
        for provider_type in [OpenAiChatCompletionsFactory::TYPE_ID, "custom-provider"] {
            let policy = pricing_policy(provider_type);
            assert!(!policy.subscription);
            assert!((policy.catalog)(&empty).provider.is_none());
            let config = DynamicConfig::from([(
                "base_url".into(),
                DynamicValue::from(" https://api.openai.com/v1/ "),
            )]);
            assert_eq!(
                (policy.catalog)(&config).provider.as_deref(),
                Some("openai")
            );
        }
    }

    #[test]
    fn pricing_basis_is_independent_of_recursive_object_key_order() -> Result<()> {
        let original = serde_json::from_str(
            r#"{"model":"model","catalog":{"tiers":[{"tier":{"type":"context","size":272000},"input":8}],"input":4},"source":"catalog"}"#,
        )?;
        let canonical = br#"{"catalog":{"input":4,"tiers":[{"input":8,"tier":{"size":272000,"type":"context"}}]},"model":"model","source":"catalog"}"#;
        let reordered = serde_json::from_slice(canonical)?;
        let expected = crate::cache::hash_chunks(&[canonical.to_vec()]);
        let original_hash = hash_pricing_basis(original)?;
        let reordered_hash = hash_pricing_basis(reordered)?;
        ensure!(original_hash == expected);
        ensure!(reordered_hash == expected);
        let changed = serde_json::json!({
            "model": "model", "source": "catalog",
            "catalog": { "input": 4, "tiers": [{ "input": 9, "tier": { "size": 272000, "type": "context" } }] }
        });
        let changed_hash = hash_pricing_basis(changed)?;
        ensure!(changed_hash != expected);
        Ok(())
    }
}
