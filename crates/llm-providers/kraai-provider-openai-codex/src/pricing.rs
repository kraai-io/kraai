use kraai_provider_core::{DynamicConfig, ProviderPricingCatalog, ProviderPricingPolicy};

pub(crate) fn pricing_policy() -> ProviderPricingPolicy {
    ProviderPricingPolicy {
        subscription: true,
        catalog,
    }
}

fn catalog(config: &DynamicConfig) -> ProviderPricingCatalog {
    ProviderPricingCatalog {
        provider: Some(String::from("openai")),
        ..ProviderPricingCatalog::from_config(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kraai_provider_core::DynamicValue;

    #[test]
    fn subscription_catalog_is_independent_of_backend() {
        let policy = pricing_policy();
        assert!(policy.subscription);
        for base_url in [None, Some("https://proxy.test/v1"), Some("  ")] {
            let config = base_url
                .map(|url| DynamicConfig::from([("base_url".into(), DynamicValue::from(url))]))
                .unwrap_or_default();
            let catalog = (policy.catalog)(&config);
            assert_eq!(catalog.provider.as_deref(), Some("openai"));
            assert_eq!(catalog.api.as_deref(), base_url.map(str::trim));
        }
    }
}
