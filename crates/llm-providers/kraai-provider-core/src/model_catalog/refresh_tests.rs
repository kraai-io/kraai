use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use color_eyre::eyre::{Result, eyre};
use kraai_types::{ModelId, ModelOptionValue, ModelOptionValues};
use serde_json::json;

use super::*;
use crate::{
    ConfiguredModelMetadata, DiscoveredModelOptions, Model, ModelCatalogView, ModelMetadataCache,
    ModelOptionsProtocol,
};

#[derive(Default)]
struct NativeModel {
    owner: String,
    options: DiscoveredModelOptions,
    config: ConfiguredModelMetadata,
}

fn snapshot(context: usize, efforts: &[&str]) -> Result<Snapshot> {
    models_snapshot(&[("model", context, efforts)])
}

fn models_snapshot(models: &[(&str, usize, &[&str])]) -> Result<Snapshot> {
    let models = models
        .iter()
        .map(|(id, context, efforts)| {
            (
                (*id).to_owned(),
                json!({
                    "limit":{"context":context},
                    "reasoning_options":[{"type":"effort","values":efforts}]
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    Ok(serde_json::from_value(json!({
        "version": CACHE_VERSION, "fetched_at":123,
        "providers":{"openai":{"models":models}}
    }))?)
}

fn resolve(
    native: &NativeModel,
    catalog: Option<&ModelCatalogView<'_>>,
) -> BTreeMap<ModelId, Model> {
    let id = ModelId::new("model");
    BTreeMap::from([(id.clone(), resolve_model(id, native, catalog))])
}

fn resolve_models(
    native: &BTreeMap<ModelId, NativeModel>,
    catalog: Option<&ModelCatalogView<'_>>,
) -> BTreeMap<ModelId, Model> {
    native
        .iter()
        .map(|(id, native)| (id.clone(), resolve_model(id.clone(), native, catalog)))
        .collect()
}

fn resolve_model(
    id: ModelId,
    native: &NativeModel,
    catalog: Option<&ModelCatalogView<'_>>,
) -> Model {
    let (mut metadata, options) = catalog
        .and_then(|catalog| {
            catalog.metadata_with_discovery(
                None,
                Some("http://proxy.test/v1"),
                id.as_str(),
                Some(&native.owner),
            )
        })
        .unwrap_or_default();
    metadata.options = native.options.definitions_with_fallback(
        ModelOptionsProtocol::OpenAiChatCompletions,
        [],
        &options,
    );
    native.config.resolve(id, Some(metadata))
}

fn discovered_models(models: &[(&str, &[&str])]) -> Result<BTreeMap<ModelId, NativeModel>> {
    models
        .iter()
        .map(|(id, efforts)| {
            Ok((
                ModelId::new(*id),
                NativeModel {
                    owner: "openai".into(),
                    options: serde_json::from_value(json!({
                        "reasoning_options":[{"type":"effort","values":efforts}]
                    }))?,
                    ..Default::default()
                },
            ))
        })
        .collect()
}

async fn cached_model(
    cache: &ModelMetadataCache<NativeModel>,
    catalog: &ModelCatalog,
) -> Result<Model> {
    cache
        .models(Some(catalog), resolve)
        .await
        .get(&ModelId::new("model"))
        .cloned()
        .ok_or_else(|| eyre!("missing model"))
}

fn check_effort(model: &Model, effort: &str) -> Result<()> {
    let mut body = json!({});
    crate::apply_model_options(
        &model.options,
        &ModelOptionValues::from([(
            "reasoning_effort".into(),
            ModelOptionValue::Choice(effort.into()),
        )]),
        &mut body,
    )?;
    assert_eq!(body, json!({"reasoning_effort":effort}));
    Ok(())
}

#[tokio::test]
async fn catalog_recovery_and_updates_refresh_cached_limits_and_options_without_rediscovery()
-> Result<()> {
    let catalog = ModelCatalog::default();
    let cache = ModelMetadataCache::default();
    let mut updates = catalog.subscribe();
    cache
        .refresh(
            Some(&catalog),
            async {
                Ok(NativeModel {
                    owner: "openai".into(),
                    ..Default::default()
                })
            },
            resolve,
        )
        .await?;
    let missing = cached_model(&cache, &catalog).await?;
    assert_eq!(missing.max_context, None);
    assert!(missing.options.is_empty());

    catalog.replace_snapshot(snapshot(4096, &["low"])?).await;
    updates.changed().await?;
    assert_eq!(
        *updates.borrow_and_update(),
        catalog.view().await.revision()
    );
    let recovered = cached_model(&cache, &catalog).await?;
    assert_eq!(recovered.max_context, Some(4096));
    check_effort(&recovered, "low")?;
    assert!(
        cache
            .refresh(
                Some(&catalog),
                async { Err(eyre!("discovery unavailable")) },
                resolve
            )
            .await
            .is_err()
    );

    catalog.replace_snapshot(snapshot(8192, &["high"])?).await;
    let updated = cached_model(&cache, &catalog).await?;
    assert_eq!(updated.max_context, Some(8192));
    check_effort(&updated, "high")?;
    assert!(check_effort(&updated, "low").is_err());

    cache
        .refresh(
            Some(&catalog),
            async {
                Ok(NativeModel {
                    owner: "openai".into(),
                    options: serde_json::from_value(
                        json!({"reasoning_options":[{"type":"effort","values":["native"]}]}),
                    )?,
                    config: ConfiguredModelMetadata {
                        max_context: Some(1024),
                        ..Default::default()
                    },
                })
            },
            resolve,
        )
        .await?;
    catalog
        .replace_snapshot(snapshot(16384, &["future"])?)
        .await;
    let overridden = cached_model(&cache, &catalog).await?;
    assert_eq!(overridden.max_context, Some(1024));
    check_effort(&overridden, "native")?;
    Ok(())
}

#[tokio::test]
async fn concurrent_readers_rebuild_each_revision_once_and_reads_continue_during_discovery()
-> Result<()> {
    let catalog = Arc::new(ModelCatalog::default());
    let cache = Arc::new(ModelMetadataCache::default());
    cache
        .refresh(
            Some(&catalog),
            async {
                Ok(NativeModel {
                    owner: "openai".into(),
                    ..Default::default()
                })
            },
            resolve,
        )
        .await?;
    catalog.replace_snapshot(snapshot(4096, &["low"])?).await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let mut readers = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let cache = cache.clone();
        let catalog = catalog.clone();
        let resolutions = resolutions.clone();
        readers.spawn(async move {
            cache
                .models(Some(&catalog), |native, catalog| {
                    resolutions.fetch_add(1, Ordering::SeqCst);
                    resolve(native, catalog)
                })
                .await
                .get(&ModelId::new("model"))
                .and_then(|model| model.max_context)
        });
    }
    while let Some(result) = readers.join_next().await {
        assert_eq!(result?, Some(4096));
    }
    assert_eq!(resolutions.load(Ordering::SeqCst), 1);

    let (started, waiting) = tokio::sync::oneshot::channel();
    let (release, ready) = tokio::sync::oneshot::channel();
    let refreshing = {
        let cache = cache.clone();
        let catalog = catalog.clone();
        tokio::spawn(async move {
            cache
                .refresh(
                    Some(&catalog),
                    async {
                        started
                            .send(())
                            .map_err(|_value| eyre!("missing refresh observer"))?;
                        ready.await?;
                        Ok(NativeModel {
                            owner: "openai".into(),
                            ..Default::default()
                        })
                    },
                    resolve,
                )
                .await
        })
    };
    waiting.await?;
    catalog.replace_snapshot(snapshot(8192, &["high"])?).await;
    let during_refresh =
        tokio::time::timeout(Duration::from_secs(1), cached_model(&cache, &catalog)).await??;
    assert_eq!(during_refresh.max_context, Some(8192));
    release
        .send(())
        .map_err(|_value| eyre!("refresh stopped"))?;
    refreshing.await??;
    check_effort(&cached_model(&cache, &catalog).await?, "high")?;
    Ok(())
}

#[tokio::test]
async fn discovery_filters_invalid_models_and_accepts_later_corrected_metadata() -> Result<()> {
    let cache = ModelMetadataCache::default();
    cache
        .refresh(
            None,
            async {
                discovered_models(&[
                    ("model", &["duplicate", "duplicate"]),
                    ("healthy", &["low"]),
                ])
            },
            resolve_models,
        )
        .await?;
    let models = cache.models(None, resolve_models).await;
    assert_eq!(models.len(), 1);
    assert!(!models.contains_key(&ModelId::new("model")));
    check_effort(
        models
            .get(&ModelId::new("healthy"))
            .ok_or_else(|| eyre!("missing healthy model"))?,
        "low",
    )?;
    drop(models);

    cache
        .refresh(
            None,
            async { discovered_models(&[("model", &["medium"]), ("healthy", &["high"])]) },
            resolve_models,
        )
        .await?;
    let models = cache.models(None, resolve_models).await;
    assert_eq!(models.len(), 2);
    check_effort(
        models
            .get(&ModelId::new("model"))
            .ok_or_else(|| eyre!("missing recovered model"))?,
        "medium",
    )?;
    check_effort(
        models
            .get(&ModelId::new("healthy"))
            .ok_or_else(|| eyre!("missing updated healthy model"))?,
        "high",
    )?;
    drop(models);
    Ok(())
}

#[tokio::test]
async fn invalid_catalog_updates_filter_only_invalid_models_and_recover_on_next_revision()
-> Result<()> {
    let catalog = ModelCatalog::default();
    let cache = ModelMetadataCache::default();
    catalog
        .replace_snapshot(models_snapshot(&[
            ("model", 4096, &["low"]),
            ("healthy", 2048, &["low"]),
        ])?)
        .await;
    cache
        .refresh(
            Some(&catalog),
            async { discovered_models(&[("model", &[]), ("healthy", &[])]) },
            resolve_models,
        )
        .await?;
    catalog
        .replace_snapshot(models_snapshot(&[
            ("model", 8192, &["duplicate", "duplicate"]),
            ("healthy", 16384, &["high"]),
        ])?)
        .await;
    let resolutions = AtomicUsize::new(0);
    for _ in 0..2 {
        let models = cache
            .models(Some(&catalog), |native, catalog| {
                resolutions.fetch_add(1, Ordering::SeqCst);
                resolve_models(native, catalog)
            })
            .await;
        assert_eq!(models.len(), 1);
        assert!(!models.contains_key(&ModelId::new("model")));
        let healthy = models
            .get(&ModelId::new("healthy"))
            .ok_or_else(|| eyre!("missing healthy model"))?;
        assert_eq!(healthy.max_context, Some(16384));
        check_effort(healthy, "high")?;
        drop(models);
    }
    assert_eq!(resolutions.load(Ordering::SeqCst), 1);
    catalog.replace_snapshot(snapshot(16384, &["high"])?).await;
    let models = cache.models(Some(&catalog), resolve_models).await;
    let recovered = models
        .get(&ModelId::new("model"))
        .ok_or_else(|| eyre!("missing recovered model"))?;
    assert_eq!(recovered.max_context, Some(16384));
    check_effort(recovered, "high")?;
    drop(models);
    Ok(())
}

#[tokio::test]
async fn invalidation_reresolves_when_replacement_catalog_has_the_same_revision() -> Result<()> {
    let previous = ModelCatalog::default();
    let replacement = ModelCatalog::default();
    previous.replace_snapshot(snapshot(4096, &["low"])?).await;
    replacement
        .replace_snapshot(snapshot(8192, &["high"])?)
        .await;
    assert_eq!(previous.revision(), replacement.revision());
    let mut cache = ModelMetadataCache::default();
    cache
        .refresh(
            Some(&previous),
            async {
                Ok(NativeModel {
                    owner: "openai".into(),
                    ..Default::default()
                })
            },
            resolve,
        )
        .await?;
    cache.invalidate();
    let updated = cached_model(&cache, &replacement).await?;
    assert_eq!(updated.max_context, Some(8192));
    check_effort(&updated, "high")?;
    Ok(())
}
