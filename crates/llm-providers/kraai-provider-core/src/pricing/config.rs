use std::collections::BTreeMap;

use color_eyre::eyre::{Result, eyre};
use kraai_types::{ModelId, TokenRates, Usd};

use crate::{
    DynamicConfig, DynamicValue, FieldDefinition, FieldValueKind, ModelConfig, ProviderConfig,
    ProviderPricingPolicy, ValidationError,
};

const RATE_FIELDS: [&str; 5] = [
    "price_input",
    "price_output",
    "price_cache_read",
    "price_cache_write",
    "price_reasoning",
];

#[derive(Clone)]
pub(super) struct PricingConfig {
    pub provider: Option<String>,
    pub api: Option<String>,
    pub subscription: bool,
    pub models: BTreeMap<ModelId, TokenRates>,
}

impl PricingConfig {
    pub fn new<'a>(
        provider: &ProviderConfig,
        models: impl IntoIterator<Item = &'a ModelConfig>,
        policy: ProviderPricingPolicy,
    ) -> Result<Self> {
        let models = models
            .into_iter()
            .filter(|model| model.provider_id == provider.id)
            .filter_map(|model| match rates(&model.config) {
                Ok(Some(rates)) => Some(Ok((model.id.clone(), rates))),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<_>>()?;
        let catalog = (policy.catalog)(&provider.config);
        let catalog_provider = provider
            .config
            .get("pricing_provider")
            .and_then(DynamicValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(String::from)
            .or(catalog.provider);
        Ok(Self {
            provider: catalog_provider,
            api: catalog.api,
            subscription: policy.subscription,
            models,
        })
    }
}

fn rate(config: &DynamicConfig, key: &str) -> Result<Option<Usd>> {
    let Some(value) = config.get(key) else {
        return Ok(None);
    };
    let value = match value {
        DynamicValue::String(value) => value.parse::<f64>().ok(),
        DynamicValue::Integer(value) => Some(*value as f64),
        DynamicValue::Bool(_) => None,
    };
    value
        .and_then(Usd::from_dollars)
        .map(Some)
        .ok_or_else(|| eyre!("{key} must be a non-negative USD price per million tokens"))
}

fn rates(config: &DynamicConfig) -> Result<Option<TokenRates>> {
    if !RATE_FIELDS.iter().any(|key| config.contains_key(*key)) {
        return Ok(None);
    }
    Ok(Some(TokenRates {
        input: rate(config, "price_input")?
            .ok_or_else(|| eyre!("price_input is required when setting prices"))?,
        output: rate(config, "price_output")?
            .ok_or_else(|| eyre!("price_output is required when setting prices"))?,
        cache_read: rate(config, "price_cache_read")?,
        cache_write: rate(config, "price_cache_write")?,
        reasoning: rate(config, "price_reasoning")?,
    }))
}

pub fn validate_pricing_config(config: &DynamicConfig) -> Vec<ValidationError> {
    match rates(config) {
        Ok(_) => Vec::new(),
        Err(error) => vec![ValidationError {
            field: String::from("price_input"),
            message: error.to_string(),
        }],
    }
}

pub fn pricing_fields(model: bool) -> Vec<FieldDefinition> {
    let fields: &[(&str, &str)] = if model {
        &[
            ("price_input", "Input USD / million tokens"),
            ("price_output", "Output USD / million tokens"),
            ("price_cache_read", "Cache read USD / million tokens"),
            ("price_cache_write", "Cache write USD / million tokens"),
            ("price_reasoning", "Reasoning USD / million tokens"),
        ]
    } else {
        &[("pricing_provider", "models.dev provider ID")]
    };
    fields.iter().map(|&(key, label)| FieldDefinition {
        key: key.into(), label: label.into(), value_kind: FieldValueKind::String,
        required: false, secret: false, default_value: None,
        help_text: Some(if model { "Decimal USD rate, for example 2.50. Input and output are both required. Missing cache rates leave cached requests unpriced." } else { "Optional provider ID for custom endpoints. Otherwise match the API base URL." }.into()),
    }).collect()
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "tests combine fallible fixture setup with assertions"
)]
mod tests {
    use super::*;

    #[test]
    fn explicit_catalog_provider_overrides_factory_policy() -> Result<()> {
        let mut provider = ProviderConfig {
            id: kraai_types::ProviderId::new("custom-name"),
            type_id: "custom-factory".into(),
            config: DynamicConfig::new(),
        };
        let policy = ProviderPricingPolicy {
            subscription: false,
            catalog: |config| crate::ProviderPricingCatalog {
                provider: Some(String::from("factory-catalog")),
                ..crate::ProviderPricingCatalog::from_config(config)
            },
        };
        assert_eq!(
            PricingConfig::new(&provider, &[], policy)?
                .provider
                .as_deref(),
            Some("factory-catalog")
        );
        provider.config.insert(
            "base_url".into(),
            DynamicValue::from("https://proxy.test/v1"),
        );
        assert_eq!(
            PricingConfig::new(&provider, &[], policy)?.api.as_deref(),
            Some("https://proxy.test/v1")
        );
        provider.config.insert(
            "pricing_provider".into(),
            DynamicValue::from("  reseller  "),
        );
        assert_eq!(
            PricingConfig::new(&provider, &[], policy)?
                .provider
                .as_deref(),
            Some("reseller")
        );
        provider
            .config
            .insert("pricing_provider".into(), DynamicValue::from("  "));
        assert_eq!(
            PricingConfig::new(&provider, &[], policy)?
                .provider
                .as_deref(),
            Some("factory-catalog")
        );
        Ok(())
    }

    #[test]
    fn subscription_is_controlled_by_policy_not_provider_type() -> Result<()> {
        let provider = ProviderConfig {
            id: kraai_types::ProviderId::new("subscription"),
            type_id: "custom-factory".into(),
            config: DynamicConfig::new(),
        };
        for subscription in [false, true] {
            let config = PricingConfig::new(
                &provider,
                &[],
                ProviderPricingPolicy {
                    subscription,
                    ..ProviderPricingPolicy::default()
                },
            )?;
            assert_eq!(config.subscription, subscription);
        }
        Ok(())
    }

    #[test]
    fn overrides_require_both_rates_and_accept_decimal_strings() -> Result<()> {
        let mut config = DynamicConfig::from([("price_input".into(), DynamicValue::from("2.50"))]);
        assert!(!validate_pricing_config(&config).is_empty());
        config.insert("price_output".into(), DynamicValue::from("10"));
        assert_eq!(
            rates(&config)?.map(|rates| rates.input),
            Some(Usd(2_500_000_000))
        );
        config.insert("price_cache_read".into(), DynamicValue::from("NaN"));
        assert!(!validate_pricing_config(&config).is_empty());
        Ok(())
    }
}
