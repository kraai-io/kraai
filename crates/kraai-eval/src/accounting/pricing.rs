use color_eyre::eyre::Result;
use kraai_provider_core::{
    DynamicConfig, DynamicValue, Pricing, ProviderConfig, ProviderFactory, ProviderManagerConfig,
};
use kraai_provider_openai_chat_completions::OpenAiFactory;
use kraai_types::{ModelId, ProviderId, RequestCost, TokenRates, TokenUsage};

use super::RequestMeasurement;

#[derive(Debug, Clone, Default)]
pub struct PricingOptions {
    pub rates: Option<TokenRates>,
}

impl PricingOptions {
    pub fn flat(rates: TokenRates) -> Self {
        Self { rates: Some(rates) }
    }
}

pub(super) enum RequestPricing {
    Flat(TokenRates),
    Catalog {
        pricing: Pricing,
        provider: ProviderId,
    },
}

impl RequestPricing {
    pub async fn load(options: &PricingOptions) -> Result<Self> {
        if let Some(rates) = &options.rates {
            return Ok(Self::Flat(rates.clone()));
        }
        let provider = ProviderId::new("benchmark");
        let config = ProviderManagerConfig {
            providers: vec![ProviderConfig {
                id: provider.clone(),
                type_id: String::from(OpenAiFactory::TYPE_ID),
                config: DynamicConfig::from([(
                    String::from("pricing_provider"),
                    DynamicValue::from("openai"),
                )]),
            }],
            models: vec![],
        };
        let pricing = Pricing::new(&config, |_| OpenAiFactory::pricing_policy())?;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), pricing.refresh()).await;
        Ok(Self::Catalog { pricing, provider })
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
        let model = ModelId::new(model);
        let quote = match self {
            Self::Flat(rates) => kraai_provider_core::PriceQuote::Configured(rates.clone()),
            Self::Catalog { pricing, provider } => pricing
                .quote(provider, &model, &model)
                .await
                .ok_or_else(|| String::from("model pricing is unavailable"))?,
        };
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

fn hash_pricing_basis(mut basis: serde_json::Value) -> serde_json::Result<String> {
    basis.sort_all_objects();
    Ok(crate::cache::hash_chunks(&[serde_json::to_vec(&basis)?]))
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre::ensure;

    use super::*;

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
