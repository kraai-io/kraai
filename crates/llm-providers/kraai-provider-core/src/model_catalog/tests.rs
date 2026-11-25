use kraai_types::{TokenUsage, Usd};
use serde_json::Value;

use super::*;

#[tokio::test]
async fn groq_endpoint_resolves_the_same_catalog_for_capabilities_and_pricing() {
    let catalog = catalog_from(serde_json::json!({
        "fetched_at": 123,
        "providers": {"groq": {"models": {"vision": {
            "modalities": {"input": ["text", "image"]},
            "cost": {"input": 1, "output": 2}
        }}}}
    }))
    .await;
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
}

#[tokio::test]
async fn matches_serving_endpoint_and_context_tier() {
    let catalog = catalog_from(serde_json::json!({
        "fetched_at": 123,
        "providers": {
            "direct": {"api":"https://direct.test/v1", "models":{"model":{"cost":{"input":2,"output":8,"cache_read":0.5,"tiers":[{"tier":{"size":200000},"input":4,"output":12,"cache_read":1}]}}}},
            "reseller": {"api":"https://reseller.test/v1", "models":{"model":{"cost":{"input":1,"output":3}}}}
        }
    }))
    .await;
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
}

#[tokio::test]
async fn catalog_rates_price_every_token_category_and_context_tier() {
    use crate::{
        DynamicConfig, ProviderConfig, ProviderManagerConfig, ProviderPricingCatalog,
        ProviderPricingPolicy, ProviderStreamEvent,
    };
    use futures::StreamExt;
    use kraai_types::{ModelId, ProviderId};

    let provider = ProviderId::new("subscription");
    let mut pricing = crate::pricing::Pricing::new(
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
    )
    .unwrap();
    pricing.catalog = std::sync::Arc::new(
        catalog_from(serde_json::json!({
            "fetched_at": 123,
            "providers": {
                "openai": {"models": {"gpt-6-astra": {"cost": {
                    "input": 10, "output": 50, "cache_read": 1, "cache_write": 12.5,
                    "tiers": [{"tier": {"type": "context", "size": 272000},
                        "input": 20, "output": 75, "cache_read": 2, "cache_write": 25}]
                }}}}
            }
        }))
        .await,
    );
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
            .apply(&provider, &ModelId::new("gpt-6-astra"), source)
            .await;
        let Some(Ok(ProviderStreamEvent::Usage(usage))) = stream.next().await else {
            panic!("missing usage");
        };
        assert_eq!(usage.cost.as_ref().map(|cost| cost.amount), Some(expected));
        assert_eq!(
            usage.cost.as_ref().map(|cost| cost.source.as_str()),
            Some("models.dev/openai/gpt-6-astra")
        );
    }
}

#[test]
fn unknown_pricing_conditions_are_not_silently_ignored() {
    let cost = serde_json::json!({"input":1,"output":2,"tiers":[{}]});
    assert!(CatalogCost::from_models_dev(&cost).is_none());
}

#[tokio::test]
async fn capabilities_are_scoped_to_the_serving_provider_and_preserve_unknowns() {
    let catalog = catalog_from(serde_json::json!({
        "fetched_at": 123,
        "providers": {
            "vision": {"api":"https://vision.test/v1", "models":{
                "shared":{"name":"Vision model","modalities":{"input":["text","image"]},"limit":{"context":65536,"output":8192}},
                "unknown":{},
                "invalid-limit":{"limit":{"context":0}}
            }},
            "text": {"api":"https://text.test/v1", "models":{"shared":{"modalities":{"input":["text"]}}}},
            "ambiguous": {"api":"https://text.test/v1", "models":{"shared":{"modalities":{"input":["text","image"]}}}}
        }
    }))
    .await;
    assert_eq!(
        catalog
            .metadata(None, Some("https://vision.test/v1/"), "shared")
            .await,
        Some(CatalogModelMetadata {
            options: Vec::new(),
            name: Some("Vision model".into()),
            max_context: Some(65536),
            max_output: Some(8192),
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
}

mod resolution;

mod pricing;

mod owner;

mod refresh;

mod equivalence;

mod store;

async fn catalog_from(snapshot: Value) -> ModelCatalog {
    let catalog = ModelCatalog::new(None);
    install(&catalog, snapshot).await;
    catalog
}

async fn install(catalog: &ModelCatalog, snapshot: Value) {
    catalog
        .install(
            serde_json::to_vec(&snapshot["providers"]).unwrap(),
            snapshot["fetched_at"].as_u64().unwrap(),
        )
        .await
        .unwrap();
}

async fn resolve(
    catalog: &ModelCatalog,
    provider: Option<&str>,
    api: Option<&str>,
    model: &str,
) -> Option<(String, String)> {
    let view = catalog.view().await;
    let resolved = view.state.reader.as_ref().and_then(|connection| {
        let serving = super::resolution::serving_provider(connection, provider, api).unwrap();
        super::resolution::model(connection, &serving, model).unwrap()
    });
    drop(view);
    resolved.map(|entry| (entry.provider, entry.model))
}
