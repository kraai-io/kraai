use super::*;
use crate::{
    DynamicConfig, DynamicValue, ModelConfig, ProviderConfig, ProviderManagerConfig,
    ProviderPricingPolicy, ProviderStreamEvent, pricing::Pricing,
};
use futures::StreamExt;
use kraai_types::{ModelId, ProviderId, RequestCost};

fn snapshot() -> color_eyre::Result<Snapshot> {
    Ok(serde_json::from_value(serde_json::json!({
        "fetched_at": 123,
        "providers": {
            "manufacturer": {"models": {"model": {
                "canonical_model_id": "manufacturer/model",
                "limit": {"context": 1000000},
                "cost": {"input": 2, "output": 8, "cache_read": 0.5,
                    "tiers": [{"tier": {"size": 200000}, "input": 4, "output": 12, "cache_read": 1}]}
            }}},
            "reseller": {"api": "https://reseller.test/v1", "models": {
                "model": {"canonical_model_id": "manufacturer/model",
                    "limit": {"context": 100000}, "cost": {"input": 9, "output": 30, "cache_read": 3}},
                "old": {"cost": {"input": 1, "output": 2}},
                "broken": {"canonical_model_id": "missing/model", "cost": {"input": 1, "output": 2}}
            }}
        }
    }))?)
}

#[tokio::test]
async fn pricing_prefers_manufacturer_without_replacing_serving_limits() -> color_eyre::Result<()> {
    for (
        endpoint,
        catalog_provider,
        pricing_provider,
        configured,
        expected_source,
        expected_input,
    ) in [
        (
            "https://reseller.test/v1",
            None,
            None,
            false,
            "models.dev/manufacturer/model",
            4,
        ),
        (
            "https://proxy.test/v1",
            None,
            None,
            false,
            "models.dev/manufacturer/model",
            4,
        ),
        (
            "https://proxy.test/v1",
            Some("reseller"),
            None,
            false,
            "models.dev/manufacturer/model",
            4,
        ),
        (
            "https://proxy.test/v1",
            Some("reseller"),
            Some("reseller"),
            false,
            "models.dev/reseller/model",
            9,
        ),
        (
            "https://reseller.test/v1",
            None,
            None,
            true,
            "configured",
            6,
        ),
    ] {
        let provider = ProviderId::new("custom");
        let model = ModelId::new("model");
        let mut config = DynamicConfig::from([("base_url".into(), DynamicValue::from(endpoint))]);
        for (key, value) in [
            ("catalog_provider", catalog_provider),
            ("pricing_provider", pricing_provider),
        ] {
            if let Some(value) = value {
                config.insert(key.into(), DynamicValue::from(value));
            }
        }
        let pricing = Pricing::new(
            &ProviderManagerConfig {
                providers: vec![ProviderConfig {
                    id: provider.clone(),
                    type_id: "custom".into(),
                    config,
                }],
                models: if configured {
                    vec![ModelConfig {
                        id: model.clone(),
                        provider_id: provider.clone(),
                        config: DynamicConfig::from([
                            ("price_input".into(), DynamicValue::from("6")),
                            ("price_output".into(), DynamicValue::from("7")),
                            ("price_cache_read".into(), DynamicValue::from("1")),
                        ]),
                    }]
                } else {
                    vec![]
                },
            },
            |_| ProviderPricingPolicy::default(),
        )?;
        *pricing.catalog.snapshot.write().await = snapshot()?;
        let quote = pricing
            .quote(&provider, &model, &model)
            .await
            .ok_or_else(|| color_eyre::eyre::eyre!("missing quote"))?;
        let usage = TokenUsage {
            input_tokens: 199999,
            cache_read_tokens: 2,
            output_tokens: 10,
            ..Default::default()
        };
        let cost = quote
            .estimate(&usage)
            .ok_or_else(|| color_eyre::eyre::eyre!("missing estimate"))?;
        assert_eq!(cost.source, expected_source);
        assert_eq!(
            cost.rates.as_ref().map(|rates| rates.input),
            Some(Usd(expected_input * 1_000_000_000))
        );
        if !configured && pricing_provider.is_none() {
            assert_eq!(cost.amount, Usd(800_118_000));
            let boundary = quote.estimate(&TokenUsage {
                cache_read_tokens: 1,
                ..usage
            });
            assert_eq!(
                boundary
                    .and_then(|cost| cost.rates)
                    .map(|rates| rates.input),
                Some(Usd(2_000_000_000))
            );
        }
        assert_eq!(
            pricing
                .catalog
                .metadata(Some("reseller"), None, "model")
                .await
                .and_then(|model| model.max_context),
            Some(100000)
        );
    }
    Ok(())
}

#[tokio::test]
async fn reported_charges_including_zero_override_catalog_estimates() -> color_eyre::Result<()> {
    let provider = ProviderId::new("custom");
    let model = ModelId::new("model");
    let pricing = Pricing::new(
        &ProviderManagerConfig {
            providers: vec![ProviderConfig {
                id: provider.clone(),
                type_id: "custom".into(),
                config: DynamicConfig::from([(
                    "base_url".into(),
                    DynamicValue::from("https://reseller.test/v1"),
                )]),
            }],
            models: vec![],
        },
        |_| ProviderPricingPolicy::default(),
    )?;
    *pricing.catalog.snapshot.write().await = snapshot()?;
    let reported = [Usd(0), Usd(123_456)].map(|amount| RequestCost {
        amount,
        source: "openrouter".into(),
        rates: None,
        priced_at: 456,
        upstream: Some(Usd(100)),
    });
    let source = futures::stream::iter(reported.clone().map(|cost| {
        Ok(ProviderStreamEvent::Usage(TokenUsage {
            input_tokens: 100,
            output_tokens: 10,
            cost: Some(cost),
            ..Default::default()
        }))
    }))
    .boxed();
    let mut stream = pricing.apply(&provider, &model, &model, source).await;
    for cost in reported {
        let Some(Ok(ProviderStreamEvent::Usage(usage))) = stream.next().await else {
            return Err(color_eyre::eyre::eyre!("missing reported usage"));
        };
        assert_eq!(usage.cost, Some(cost));
    }
    Ok(())
}

#[tokio::test]
async fn missing_canonical_identity_retains_provenance_but_broken_identity_is_unknown()
-> color_eyre::Result<()> {
    let catalog = ModelCatalog::default();
    *catalog.snapshot.write().await = snapshot()?;
    assert_eq!(
        catalog
            .lookup(
                Some("reseller"),
                None,
                "old",
                CatalogPricingSource::Manufacturer
            )
            .await
            .map(|(_, source, _)| source),
        Some("models.dev/reseller/old".into())
    );
    assert!(
        catalog
            .lookup(
                Some("reseller"),
                None,
                "broken",
                CatalogPricingSource::Manufacturer
            )
            .await
            .is_none()
    );
    assert!(
        catalog
            .lookup(
                Some("reseller"),
                None,
                "broken",
                CatalogPricingSource::Provider
            )
            .await
            .is_some()
    );
    Ok(())
}
