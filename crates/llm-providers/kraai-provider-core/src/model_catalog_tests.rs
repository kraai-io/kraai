#![expect(clippy::panic_in_result_fn, reason = "tests assert catalog fixtures")]
use super::*;

#[tokio::test]
async fn groq_endpoint_resolves_the_same_catalog_for_capabilities_and_pricing()
-> color_eyre::Result<()> {
    let catalog = ModelCatalog::default();
    *catalog.snapshot.write().await = serde_json::from_value(serde_json::json!({
        "fetched_at": 123,
        "providers": {"groq": {"models": {"vision": {
            "modalities": {"input": ["text", "image"]},
            "cost": {"input": 1, "output": 2}
        }}}}
    }))?;
    for endpoint in [
        "https://api.groq.com/openai/v1",
        "https://api.groq.com/openai/v1/",
    ] {
        assert_eq!(
            catalog
                .metadata(None, Some(endpoint), "vision")
                .await
                .and_then(|model| model.supports_images),
            Some(true)
        );
        assert_eq!(
            catalog
                .lookup(
                    None,
                    Some(endpoint),
                    "vision",
                    CatalogPricingSource::Manufacturer
                )
                .await
                .map(|(_, source, _)| source),
            Some("models.dev/groq/vision".into())
        );
    }
    for endpoint in ["https://proxy.test/openai/v1", "https://api.groq.com/other"] {
        assert!(
            catalog
                .metadata(None, Some(endpoint), "vision")
                .await
                .is_some()
        );
        assert!(
            catalog
                .lookup(
                    None,
                    Some(endpoint),
                    "vision",
                    CatalogPricingSource::Manufacturer
                )
                .await
                .is_some()
        );
    }
    assert!(
        catalog
            .metadata(
                Some("missing"),
                Some("https://api.groq.com/openai/v1"),
                "vision"
            )
            .await
            .is_none()
    );
    Ok(())
}

#[test]
fn cached_snapshot_preserves_payload_and_rejects_invalid_data() -> color_eyre::Result<()> {
    let value = serde_json::json!({
        "version": CACHE_VERSION,
        "fetched_at": 123,
        "providers": {
            "fixture": {
                "api": "https://fixture.test/v1",
                "models": {"model": {"canonical_model_id":"fixture/model","cost": {"input": 2, "output": 8}}}
            }
        }
    });
    let encoded = serde_json::to_vec(&value)?;
    let snapshot = read_cached_snapshot(encoded.as_slice())
        .ok_or_else(|| color_eyre::eyre::eyre!("valid snapshot was rejected"))?;
    assert_eq!(serde_json::to_value(snapshot)?, value);
    assert!(read_cached_snapshot(b"{\"fetched_at\":".as_slice()).is_none());
    assert!(read_cached_snapshot(b"not json".as_slice()).is_none());
    Ok(())
}

#[test]
fn cached_snapshot_accepts_the_limit_and_bounds_larger_reads() {
    let prefix = br#"{"fetched_at":123,"providers":{}}"#;
    let at_limit = prefix
        .as_slice()
        .chain(std::io::repeat(b' '))
        .take(MAX_BYTES as u64);
    assert!(read_cached_snapshot(at_limit).is_some());
    let mut growing = prefix
        .as_slice()
        .chain(std::io::repeat(b' '))
        .take(MAX_BYTES as u64 * 2);
    assert!(read_cached_snapshot(&mut growing).is_none());
    assert_eq!(growing.limit(), MAX_BYTES as u64 - 1);
}

#[tokio::test]
async fn matches_serving_endpoint_and_context_tier() -> color_eyre::Result<()> {
    let catalog = ModelCatalog::default();
    *catalog.snapshot.write().await = serde_json::from_value(serde_json::json!({
        "fetched_at": 123,
        "providers": {
            "direct": {"api":"https://direct.test/v1", "models":{"model":{"cost":{"input":2,"output":8,"cache_read":0.5,"tiers":[{"tier":{"size":200000},"input":4,"output":12,"cache_read":1}]}}}},
            "reseller": {"api":"https://reseller.test/v1", "models":{"model":{"cost":{"input":1,"output":3}}}}
        }
    }))?;
    let usage = TokenUsage {
        input_tokens: 200_000,
        cache_read_tokens: 1,
        ..Default::default()
    };
    let direct = catalog
        .lookup(
            None,
            Some("https://direct.test/v1/"),
            "model",
            CatalogPricingSource::Manufacturer,
        )
        .await;
    assert!(direct.is_some_and(|(cost, source, timestamp)| {
        parse_rates(&cost, &usage).is_some_and(|rates| rates.input == Usd(4_000_000_000))
            && source == "models.dev/direct/model"
            && timestamp == 123
    }));
    let reseller = catalog
        .lookup(
            Some("reseller"),
            None,
            "model",
            CatalogPricingSource::Provider,
        )
        .await;
    assert!(reseller.is_some_and(|(cost, _, _)| {
        parse_rates(&cost, &usage).is_some_and(|rates| rates.input == Usd(1_000_000_000))
    }));
    assert!(
        catalog
            .lookup(
                None,
                Some("https://unknown.test/v1"),
                "model",
                CatalogPricingSource::Manufacturer
            )
            .await
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn reasoning_variant_uses_base_catalog_rates_for_all_token_categories()
-> color_eyre::Result<()> {
    use crate::{
        DynamicConfig, ProviderConfig, ProviderManagerConfig, ProviderPricingCatalog,
        ProviderPricingPolicy, ProviderStreamEvent,
    };
    use futures::StreamExt;
    use kraai_types::{ModelId, ProviderId};

    let provider = ProviderId::new("subscription");
    let pricing = crate::pricing::Pricing::new(
        &ProviderManagerConfig {
            providers: vec![ProviderConfig {
                id: provider.clone(),
                type_id: "custom-factory".into(),
                config: DynamicConfig::new(),
            }],
            models: vec![],
        },
        |_| ProviderPricingPolicy {
            subscription: true,
            catalog: |_| ProviderPricingCatalog {
                api: None,
                provider: Some("openai".into()),
            },
        },
    )?;
    *pricing.catalog.snapshot.write().await = serde_json::from_value(serde_json::json!({
        "fetched_at": 123,
        "providers": {
            "openai": {"models": {"gpt-6-astra": {"cost": {
                "input": 10, "output": 50, "cache_read": 1, "cache_write": 12.5,
                "tiers": [{"tier": {"type": "context", "size": 272000},
                    "input": 20, "output": 75, "cache_read": 2, "cache_write": 25}]
            }}}}
        }
    }))?;
    for (input_tokens, expected) in [(100, Usd(9_025_000)), (272001, Usd(5_452_320_000))] {
        let source = futures::stream::iter([Ok(ProviderStreamEvent::Usage(TokenUsage {
            input_tokens,
            cache_read_tokens: 400,
            cache_write_tokens: 10,
            output_tokens: 100,
            reasoning_tokens: 50,
            ..Default::default()
        }))])
        .boxed();
        let mut stream = pricing
            .apply(
                &provider,
                &ModelId::new("gpt-6-astra-low"),
                &ModelId::new("gpt-6-astra"),
                source,
            )
            .await;
        let Some(Ok(ProviderStreamEvent::Usage(usage))) = stream.next().await else {
            return Err(color_eyre::eyre::eyre!("missing usage"));
        };
        assert_eq!(usage.cost.as_ref().map(|cost| cost.amount), Some(expected));
        assert_eq!(
            usage.cost.as_ref().map(|cost| cost.source.as_str()),
            Some("models.dev/openai/gpt-6-astra")
        );
    }
    Ok(())
}

#[test]
fn unknown_pricing_conditions_are_not_silently_ignored() {
    let cost = serde_json::json!({"input":1,"output":2,"tiers":[{}]});
    assert!(parse_rates(&cost, &TokenUsage::default()).is_none());
}

#[tokio::test]
async fn capabilities_are_scoped_to_the_serving_provider_and_preserve_unknowns()
-> color_eyre::Result<()> {
    let catalog = ModelCatalog::default();
    *catalog.snapshot.write().await = serde_json::from_value(serde_json::json!({
        "fetched_at": 123,
        "providers": {
            "vision": {"api":"https://vision.test/v1", "models":{
                "shared":{"name":"Vision model","modalities":{"input":["text","image"]},"limit":{"context":65536}},
                "unknown":{},
                "invalid-limit":{"limit":{"context":0}}
            }},
            "text": {"api":"https://text.test/v1", "models":{"shared":{"modalities":{"input":["text"]}}}},
            "ambiguous": {"api":"https://text.test/v1", "models":{"shared":{"modalities":{"input":["text","image"]}}}}
        }
    }))?;
    assert_eq!(
        catalog
            .metadata(None, Some("https://vision.test/v1/"), "shared")
            .await,
        Some(CatalogModelMetadata {
            options: Vec::new(),
            name: Some("Vision model".into()),
            max_context: Some(65536),
            supports_images: Some(true)
        })
    );
    assert_eq!(
        catalog
            .metadata(Some("text"), None, "shared")
            .await
            .and_then(|model| model.supports_images),
        Some(false)
    );
    assert_eq!(
        catalog
            .metadata(None, Some("https://vision.test/v1"), "unknown")
            .await,
        Some(CatalogModelMetadata::default())
    );
    assert_eq!(
        catalog
            .metadata(Some("vision"), None, "invalid-limit")
            .await
            .and_then(|model| model.max_context),
        None
    );
    assert!(
        catalog
            .metadata(None, Some("https://text.test/v1"), "shared")
            .await
            .is_none()
    );
    assert!(
        catalog
            .metadata(None, Some("https://unknown.test/v1"), "shared")
            .await
            .is_none()
    );
    assert!(
        catalog
            .metadata(Some("missing"), Some("https://vision.test/v1"), "shared")
            .await
            .is_none()
    );
    Ok(())
}

#[path = "model_catalog/resolution_tests.rs"]
mod resolution_tests;

#[path = "model_catalog/pricing_tests.rs"]
mod pricing_tests;
