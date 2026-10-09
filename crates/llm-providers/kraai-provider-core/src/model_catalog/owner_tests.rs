use super::*;
use crate::{DiscoveredModelOptions, ModelOptionsProtocol};
use kraai_types::{ModelOptionValue, ModelOptionValues};
use serde_json::json;

async fn proxy_catalog() -> color_eyre::Result<ModelCatalog> {
    let catalog = ModelCatalog::default();
    *catalog.snapshot.write().await = serde_json::from_value(json!({
        "version": CACHE_VERSION, "fetched_at": 123,
        "providers": {
            "openai": {"name":"OpenAI", "models": {
                "gpt-6.1-sol": {
                    "canonical_model_id":"openai/gpt-6.1-sol",
                    "limit":{"context":1050000},
                    "reasoning_options":[{"type":"effort","values":["low","high","max"]}],
                    "options":[{
                        "id":"native_only","label":"Native only","type":"boolean",
                        "binding":{"type":"body","path":"/native_only"}
                    }],
                    "experimental":{"modes":{"fast":{"provider":{
                        "body":{"service_tier":"priority"},"headers":{"x-native":"true"}
                    }}}}
                },
                "gpt-oss-120b": {"limit":{"context":128000}}
            }},
            "groq": {"models": {"openai/gpt-oss-120b": {
                "canonical_model_id":"openai/gpt-oss-120b",
                "limit":{"context":131072},
                "reasoning_options":[{"type":"effort","values":["low","medium","high"]}]
            }}},
            "opencode": {"name":"OpenCode Zen", "models": {"space-bunny-free": {
                "limit":{"context":1048576},
                "reasoning_options":[{"type":"effort","values":["low","high","max"]}]
            }}},
            "reseller": {"api":"https://reseller.test/v1", "models": {"gpt-6.1-sol": {
                "canonical_model_id":"openai/gpt-6.1-sol",
                "limit":{"context":4096},
                "reasoning_options":[{"type":"effort","values":["host-effort"]}]
            }}}
        }
    }))?;
    Ok(catalog)
}

#[tokio::test]
async fn proxy_model_owners_supply_context_and_protocol_reasoning_without_native_patches()
-> color_eyre::Result<()> {
    let catalog = proxy_catalog().await?;
    for (id, owner, context, effort) in [
        ("gpt-6.1-sol", "openai", 1050000, "max"),
        ("openai/gpt-oss-120b", "groq", 131072, "medium"),
        ("space-bunny-free", "opencode zen", 1048576, "max"),
    ] {
        let (metadata, discovery) = catalog
            .metadata_with_discovery(None, Some("http://proxy.test/v1"), id, Some(owner))
            .await
            .ok_or_else(|| color_eyre::eyre::eyre!("missing owner metadata"))?;
        assert_eq!(metadata.max_context, Some(context));
        let options = discovery.definitions(ModelOptionsProtocol::OpenAiChatCompletions);
        assert_eq!(options.len(), 1);
        let mut body = json!({"model": id});
        let headers = crate::apply_model_options(
            &options,
            &ModelOptionValues::from([(
                "reasoning_effort".into(),
                ModelOptionValue::Choice(effort.into()),
            )]),
            &mut body,
        )?;
        assert_eq!(body, json!({"model":id,"reasoning_effort":effort}));
        assert!(headers.is_empty());
        let native: DiscoveredModelOptions = serde_json::from_value(json!({
            "reasoning_options":[{"type":"effort","values":["proxy-effort"]}]
        }))?;
        let options = native.definitions_with_fallback(
            ModelOptionsProtocol::OpenAiChatCompletions,
            [],
            &discovery,
        );
        crate::apply_model_options(
            &options,
            &ModelOptionValues::from([(
                "reasoning_effort".into(),
                ModelOptionValue::Choice("proxy-effort".into()),
            )]),
            &mut body,
        )?;
        assert_eq!(body.get("reasoning_effort"), Some(&json!("proxy-effort")));
    }
    Ok(())
}

#[tokio::test]
async fn advertised_owners_do_not_override_known_endpoints_or_configured_catalogs()
-> color_eyre::Result<()> {
    let catalog = proxy_catalog().await?;
    for (provider, endpoint) in [
        (Some("reseller"), "http://proxy.test/v1"),
        (None, "https://reseller.test/v1"),
    ] {
        let (metadata, discovery) = catalog
            .metadata_with_discovery(provider, Some(endpoint), "gpt-6.1-sol", Some("openai"))
            .await
            .ok_or_else(|| color_eyre::eyre::eyre!("missing serving metadata"))?;
        assert_eq!(metadata.max_context, Some(4096));
        let mut body = json!({});
        crate::apply_model_options(
            &discovery.definitions(ModelOptionsProtocol::OpenAiChatCompletions),
            &ModelOptionValues::from([(
                "reasoning_effort".into(),
                ModelOptionValue::Choice("host-effort".into()),
            )]),
            &mut body,
        )?;
        assert_eq!(body, json!({"reasoning_effort":"host-effort"}));
        assert!(
            catalog
                .metadata_with_discovery(
                    provider,
                    Some(endpoint),
                    "space-bunny-free",
                    Some("opencode")
                )
                .await
                .is_none()
        );
    }
    assert!(
        catalog
            .metadata_with_discovery(Some("missing"), None, "gpt-6.1-sol", Some("openai"))
            .await
            .is_none()
    );
    for owner in [None, Some("unknown"), Some("system")] {
        let (metadata, discovery) = catalog
            .metadata_with_discovery(None, Some("http://proxy.test/v1"), "gpt-6.1-sol", owner)
            .await
            .ok_or_else(|| color_eyre::eyre::eyre!("missing canonical metadata"))?;
        assert_eq!(metadata.max_context, Some(1050000));
        assert!(
            discovery
                .definitions(ModelOptionsProtocol::OpenAiChatCompletions)
                .is_empty()
        );
    }
    Ok(())
}

#[tokio::test]
async fn provider_display_names_require_unique_matches_and_survive_caching()
-> color_eyre::Result<()> {
    let catalog = proxy_catalog().await?;
    let encoded = serde_json::to_value(&*catalog.snapshot.read().await)?;
    assert_eq!(
        encoded.pointer("/providers/opencode/name"),
        Some(&json!("OpenCode Zen"))
    );
    let mut snapshot: Snapshot = serde_json::from_value(encoded)?;
    assert!(
        snapshot
            .owned_model(None, None, "space-bunny-free", Some("OpenCode Zen"))
            .is_some()
    );
    snapshot.providers.insert(
        "other".into(),
        serde_json::from_value(json!({
            "name":"OpenCode Zen", "models":{}
        }))?,
    );
    assert!(
        snapshot
            .owned_model(None, None, "space-bunny-free", Some("OpenCode Zen"))
            .is_none()
    );
    assert!(
        snapshot
            .owned_model(None, None, "space-bunny-free", Some("opencode"))
            .is_some()
    );
    Ok(())
}
