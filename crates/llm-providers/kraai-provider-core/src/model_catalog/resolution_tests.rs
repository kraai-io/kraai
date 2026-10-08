use super::*;

#[tokio::test]
async fn proxy_models_resolve_canonical_metadata_and_rates() -> color_eyre::Result<()> {
    let catalog = ModelCatalog::default();
    *catalog.snapshot.write().await = serde_json::from_value(serde_json::json!({
        "fetched_at": 123,
        "providers": {
            "openai": {"models": {"gpt-6.1-sol": {
                "canonical_model_id": "openai/gpt-6.1-sol",
                "limit": {"context": 400000}, "modalities": {"input": ["text", "image"]},
                "cost": {"input": 2, "output": 8}
            }}},
            "anthropic": {"models": {"claude": {
                "canonical_model_id": "anthropic/claude",
                "limit": {"context": 200000}, "cost": {"input": 3, "output": 15}
            }}},
            "reseller": {"api": "https://reseller.test/v1", "models": {
                "gpt-6.1-sol": {"canonical_model_id": "openai/gpt-6.1-sol",
                    "limit": {"context": 1000}, "cost": {"input": 99, "output": 99}},
                "claude": {"canonical_model_id": "anthropic/claude"}
            }}
        }
    }))?;
    for (model, context, source, input) in [
        ("gpt-6.1-sol", 400000, "openai/gpt-6.1-sol", 2),
        ("openai/gpt-6.1-sol", 400000, "openai/gpt-6.1-sol", 2),
        ("claude", 200000, "anthropic/claude", 3),
    ] {
        assert_eq!(
            catalog
                .metadata(None, Some("http://localhost:8080/v1"), model)
                .await
                .and_then(|metadata| metadata.max_context),
            Some(context)
        );
        let result = catalog
            .lookup(
                None,
                Some("http://localhost:8080/v1"),
                model,
                CatalogPricingSource::Manufacturer,
            )
            .await;
        assert!(result.is_some_and(|(cost, actual_source, timestamp)| {
            cost.get("input") == Some(&serde_json::json!(input))
                && actual_source == format!("models.dev/{source}")
                && timestamp == 123
        }));
    }
    for (provider, api) in [
        (Some("reseller"), Some("http://localhost:8080/v1")),
        (None, Some("https://reseller.test/v1")),
    ] {
        assert_eq!(
            catalog
                .metadata(provider, api, "gpt-6.1-sol")
                .await
                .and_then(|metadata| metadata.max_context),
            Some(1000)
        );
        assert!(
            catalog
                .lookup(provider, api, "gpt-6.1-sol", CatalogPricingSource::Provider)
                .await
                .is_some_and(
                    |(cost, source, _)| cost.get("input") == Some(&serde_json::json!(99))
                        && source == "models.dev/reseller/gpt-6.1-sol"
                )
        );
        assert!(
            catalog
                .metadata(provider, api, "openai/gpt-6.1-sol")
                .await
                .is_none()
        );
        assert!(
            catalog
                .lookup(
                    provider,
                    api,
                    "openai/gpt-6.1-sol",
                    CatalogPricingSource::Manufacturer
                )
                .await
                .is_none()
        );
    }
    Ok(())
}

#[test]
fn inference_rejects_conflicting_or_unresolved_canonical_models() -> color_eyre::Result<()> {
    let snapshot: Snapshot = serde_json::from_value(serde_json::json!({
        "fetched_at": 123,
        "providers": {
            "one": {"api": "https://shared.test/v1", "models": {
                "conflict": {"canonical_model_id": "one/conflict"},
                "unresolved": {"canonical_model_id": "missing/model"},
                "indirect": {"canonical_model_id": "two/alias"},
                "unique": {},
                "mixed": {"canonical_model_id": "one/mixed"}
            }},
            "two": {"api": "https://shared.test/v1", "models": {
                "conflict": {"canonical_model_id": "two/conflict"},
                "alias": {"canonical_model_id": "one/unique"},
                "mixed": {}
            }}
        }
    }))?;
    for model in ["conflict", "unresolved", "indirect", "mixed", "unknown"] {
        assert!(
            snapshot
                .model(None, Some("https://proxy.test/v1"), model)
                .is_none()
        );
    }
    assert!(snapshot.model(None, None, "unique").is_some());
    assert!(
        snapshot
            .model(None, Some("https://shared.test/v1"), "unique")
            .is_none()
    );
    assert!(snapshot.model(Some("missing"), None, "unique").is_none());
    assert!(snapshot.model(Some("two"), None, "unique").is_none());
    assert!(
        snapshot
            .model(None, Some("https://api.groq.com/openai/v1"), "unique")
            .is_none()
    );
    Ok(())
}

#[test]
fn old_cache_retains_data_but_requires_refresh() -> color_eyre::Result<()> {
    let mut snapshot: Snapshot = serde_json::from_value(serde_json::json!({
        "fetched_at": 100,
        "providers": {"provider": {"models": {"model": {"cost": {"input": 2, "output": 3}}}}}
    }))?;
    assert!(!snapshot.is_fresh(101));
    assert!(snapshot.model(Some("provider"), None, "model").is_some());
    snapshot.version = CACHE_VERSION;
    assert!(snapshot.is_fresh(101));
    assert!(!snapshot.is_fresh(100 + MAX_AGE));
    assert!(!snapshot.is_fresh(99));
    Ok(())
}

#[tokio::test]
async fn reasoning_options_and_modes_follow_the_serving_provider_without_manufacturer_fallback()
-> color_eyre::Result<()> {
    let catalog = ModelCatalog::default();
    *catalog.snapshot.write().await = serde_json::from_value(serde_json::json!({
        "version": CACHE_VERSION,
        "fetched_at": 123,
        "providers": {
            "manufacturer":{"models":{"model":{
                "canonical_model_id":"manufacturer/model",
                "name":"Canonical","limit":{"context":400000},
                "reasoning_options":[{"type":"effort","values":["manufacturer-effort"]}],
                "experimental":{"modes":{"manufacturer-mode":{"provider":{"body":{"speed":"manufacturer"}}}}}
            }}},
            "reseller":{"api":"https://reseller.test/v1","models":{"model":{
                "canonical_model_id":"manufacturer/model","name":"Reseller",
                "reasoning_options":[{"type":"effort","values":[null,"host-effort"]}],
                "experimental":{"modes":{"host-mode":{"provider":{"body":{"speed":"host"},"headers":{"x-mode":"host"}}}}}
            }}}
        }
    }))?;
    let unknown = catalog
        .metadata_for_protocol(
            None,
            Some("https://unknown.test/v1"),
            "manufacturer/model",
            crate::ModelOptionsProtocol::OpenAiChatCompletions,
        )
        .await
        .ok_or_else(|| color_eyre::eyre::eyre!("missing canonical metadata"))?;
    assert_eq!(unknown.name.as_deref(), Some("Canonical"));
    assert!(unknown.options.is_empty());
    let host = catalog
        .metadata_for_protocol(
            None,
            Some("https://reseller.test/v1"),
            "model",
            crate::ModelOptionsProtocol::OpenAiChatCompletions,
        )
        .await
        .ok_or_else(|| color_eyre::eyre::eyre!("missing host metadata"))?;
    let mut body = serde_json::json!({});
    let headers = crate::apply_model_options(
        &host.options,
        &kraai_types::ModelOptionValues::from([
            (
                "reasoning_effort".into(),
                kraai_types::ModelOptionValue::Choice("host-effort".into()),
            ),
            (
                "mode:host-mode".into(),
                kraai_types::ModelOptionValue::Boolean(true),
            ),
        ]),
        &mut body,
    )?;
    assert_eq!(
        body,
        serde_json::json!({"reasoning_effort":"host-effort","speed":"host"})
    );
    assert_eq!(headers.get("x-mode").map(String::as_str), Some("host"));
    let encoded = serde_json::to_value(&*catalog.snapshot.read().await)?;
    assert_eq!(
        encoded.pointer("/providers/reseller/models/model/reasoning_options/0/values"),
        Some(&serde_json::json!([null, "host-effort"]))
    );
    assert_eq!(
        encoded.pointer(
            "/providers/reseller/models/model/experimental/modes/host-mode/provider/body/speed"
        ),
        Some(&serde_json::json!("host"))
    );
    Ok(())
}
