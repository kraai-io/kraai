use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kraai_types::{ModelId, ModelOptionValue, ModelOptionValues};
use serde_json::json;

use super::*;
use crate::{
    ConfiguredModelMetadata, DiscoveredModelOptions, Model, ModelMetadataCache,
    ModelOptionsProtocol,
};

#[derive(Default)]
struct NativeModel {
    owner: String,
    options: DiscoveredModelOptions,
    config: ConfiguredModelMetadata,
}

fn snapshot(context: usize, efforts: &[&str]) -> Value {
    models_snapshot(&[("model", context, efforts)])
}

fn models_snapshot(models: &[(&str, usize, &[&str])]) -> Value {
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
    json!({
        "fetched_at":123,
        "providers":{"openai":{"models":models}}
    })
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

fn discovered_models(models: &[(&str, &[&str])]) -> BTreeMap<ModelId, NativeModel> {
    models
        .iter()
        .map(|(id, efforts)| {
            (
                ModelId::new(*id),
                NativeModel {
                    owner: "openai".into(),
                    options: serde_json::from_value(json!({
                        "reasoning_options":[{"type":"effort","values":efforts}]
                    }))
                    .unwrap(),
                    ..Default::default()
                },
            )
        })
        .collect()
}

async fn cached_model(cache: &ModelMetadataCache<NativeModel>, catalog: &ModelCatalog) -> Model {
    cache
        .models(Some(catalog), resolve)
        .await
        .get(&ModelId::new("model"))
        .cloned()
        .expect("missing model")
}

fn check_effort(model: &Model, effort: &str) {
    let mut body = json!({});
    crate::apply_model_options(
        &model.options,
        &ModelOptionValues::from([(
            "reasoning_effort".into(),
            ModelOptionValue::Choice(effort.into()),
        )]),
        &mut body,
    )
    .unwrap();
    assert_eq!(body, json!({"reasoning_effort":effort}));
}

#[tokio::test]
async fn catalog_recovery_and_updates_refresh_cached_limits_and_options_without_rediscovery() {
    let catalog = ModelCatalog::new(None);
    let cache = ModelMetadataCache::default();
    let mut updates = catalog.subscribe();
    cache
        .install(
            Some(&catalog),
            NativeModel {
                owner: "openai".into(),
                ..Default::default()
            },
            resolve,
        )
        .await;
    let missing = cached_model(&cache, &catalog).await;
    assert_eq!(missing.max_context, None);
    assert!(crate::model_options::only_optional_reasoning(
        &missing.options,
        ModelOptionsProtocol::OpenAiChatCompletions
    ));

    install(&catalog, snapshot(4096, &["low"])).await;
    updates.changed().await.unwrap();
    assert_eq!(
        *updates.borrow_and_update(),
        catalog.view().await.revision()
    );
    let recovered = cached_model(&cache, &catalog).await;
    assert_eq!(recovered.max_context, Some(4096));
    check_effort(&recovered, "low");

    install(&catalog, snapshot(8192, &["high"])).await;
    let updated = cached_model(&cache, &catalog).await;
    assert_eq!(updated.max_context, Some(8192));
    check_effort(&updated, "high");
    assert!(updated.options.iter().any(|option| {
        option.id == "reasoning_effort"
            && matches!(
                &option.kind,
                kraai_types::ModelOptionKind::Choice { choices }
                    if choices.iter().map(|choice| choice.id.as_str()).eq(["high"])
            )
    }));

    cache
        .install(
            Some(&catalog),
            NativeModel {
                owner: "openai".into(),
                options: serde_json::from_value(
                    json!({"reasoning_options":[{"type":"effort","values":["native"]}]}),
                )
                .unwrap(),
                config: ConfiguredModelMetadata {
                    max_context: Some(1024),
                    ..Default::default()
                },
            },
            resolve,
        )
        .await;
    install(&catalog, snapshot(16384, &["future"])).await;
    let overridden = cached_model(&cache, &catalog).await;
    assert_eq!(overridden.max_context, Some(1024));
    check_effort(&overridden, "native");
}

#[tokio::test]
async fn concurrent_readers_rebuild_each_revision_once() {
    let catalog = Arc::new(ModelCatalog::new(None));
    let cache = Arc::new(ModelMetadataCache::default());
    cache
        .install(
            Some(&catalog),
            NativeModel {
                owner: "openai".into(),
                ..Default::default()
            },
            resolve,
        )
        .await;
    install(&catalog, snapshot(4096, &["low"])).await;
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
        assert_eq!(result.unwrap(), Some(4096));
    }
    assert_eq!(resolutions.load(Ordering::SeqCst), 1);

    install(&catalog, snapshot(8192, &["high"])).await;
    let updated = cached_model(&cache, &catalog).await;
    assert_eq!(updated.max_context, Some(8192));
    check_effort(&updated, "high");
}

#[tokio::test]
async fn discovery_filters_invalid_models_and_accepts_later_corrected_metadata() {
    let cache = ModelMetadataCache::default();
    cache
        .install(
            None,
            discovered_models(&[
                ("model", &["duplicate", "duplicate"]),
                ("healthy", &["low"]),
            ]),
            resolve_models,
        )
        .await;
    let models = cache.models(None, resolve_models).await;
    assert_eq!(models.len(), 1);
    assert!(!models.contains_key(&ModelId::new("model")));
    check_effort(
        models
            .get(&ModelId::new("healthy"))
            .expect("missing healthy model"),
        "low",
    );
    drop(models);

    cache
        .install(
            None,
            discovered_models(&[("model", &["medium"]), ("healthy", &["high"])]),
            resolve_models,
        )
        .await;
    let models = cache.models(None, resolve_models).await;
    assert_eq!(models.len(), 2);
    check_effort(
        models
            .get(&ModelId::new("model"))
            .expect("missing recovered model"),
        "medium",
    );
    check_effort(
        models
            .get(&ModelId::new("healthy"))
            .expect("missing updated healthy model"),
        "high",
    );
    drop(models);
}

#[tokio::test]
async fn invalid_catalog_updates_filter_only_invalid_models_and_recover_on_next_revision() {
    let catalog = ModelCatalog::new(None);
    let cache = ModelMetadataCache::default();
    install(
        &catalog,
        models_snapshot(&[("model", 4096, &["low"]), ("healthy", 2048, &["low"])]),
    )
    .await;
    cache
        .install(
            Some(&catalog),
            discovered_models(&[("model", &[]), ("healthy", &[])]),
            resolve_models,
        )
        .await;
    install(
        &catalog,
        models_snapshot(&[
            ("model", 8192, &["duplicate", "duplicate"]),
            ("healthy", 16384, &["high"]),
        ]),
    )
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
            .expect("missing healthy model");
        assert_eq!(healthy.max_context, Some(16384));
        check_effort(healthy, "high");
        drop(models);
    }
    assert_eq!(resolutions.load(Ordering::SeqCst), 1);
    install(&catalog, snapshot(16384, &["high"])).await;
    let models = cache.models(Some(&catalog), resolve_models).await;
    let recovered = models
        .get(&ModelId::new("model"))
        .expect("missing recovered model");
    assert_eq!(recovered.max_context, Some(16384));
    check_effort(recovered, "high");
    drop(models);
}

#[tokio::test]
async fn invalidation_reresolves_when_replacement_catalog_has_the_same_revision() {
    let previous = ModelCatalog::new(None);
    let replacement = ModelCatalog::new(None);
    install(&previous, snapshot(4096, &["low"])).await;
    install(&replacement, snapshot(8192, &["high"])).await;
    assert_eq!(previous.revision(), replacement.revision());
    let mut cache = ModelMetadataCache::default();
    cache
        .install(
            Some(&previous),
            NativeModel {
                owner: "openai".into(),
                ..Default::default()
            },
            resolve,
        )
        .await;
    cache.invalidate();
    let updated = cached_model(&cache, &replacement).await;
    assert_eq!(updated.max_context, Some(8192));
    check_effort(&updated, "high");
}
