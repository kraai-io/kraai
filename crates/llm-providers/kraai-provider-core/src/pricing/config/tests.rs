use super::*;

#[test]
fn explicit_catalog_provider_overrides_factory_policy() {
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
        PricingConfig::new(&provider, &[], policy)
            .unwrap()
            .provider
            .as_deref(),
        Some("factory-catalog")
    );
    provider.config.insert(
        "base_url".into(),
        DynamicValue::from("https://proxy.test/v1"),
    );
    assert_eq!(
        PricingConfig::new(&provider, &[], policy)
            .unwrap()
            .api
            .as_deref(),
        Some("https://proxy.test/v1")
    );
    provider.config.insert(
        "pricing_provider".into(),
        DynamicValue::from("  reseller  "),
    );
    assert_eq!(
        PricingConfig::new(&provider, &[], policy)
            .unwrap()
            .provider
            .as_deref(),
        Some("reseller")
    );
    provider
        .config
        .insert("pricing_provider".into(), DynamicValue::from("  "));
    assert_eq!(
        PricingConfig::new(&provider, &[], policy)
            .unwrap()
            .provider
            .as_deref(),
        Some("factory-catalog")
    );
}

#[test]
fn overrides_require_both_rates_and_accept_decimal_strings() {
    let mut config = DynamicConfig::from([("price_input".into(), DynamicValue::from("2.50"))]);
    assert!(!validate_pricing_config(&config).is_empty());
    config.insert("price_output".into(), DynamicValue::from("10"));
    assert_eq!(
        rates(&config).unwrap().map(|rates| rates.input),
        Some(Usd(2_500_000_000))
    );
    config.insert("price_cache_read".into(), DynamicValue::from("NaN"));
    assert!(!validate_pricing_config(&config).is_empty());
}
