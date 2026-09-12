#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert discovery behavior and propagate fixture errors"
)]

use super::*;
use crate::wire::ListModelsResponse;
use serde_json::json;

fn model(slug: &str) -> Result<ListModelEntry> {
    Ok(serde_json::from_value(json!({
        "slug": slug,
        "display_name": "New Codex Model",
        "visibility": "list",
        "context_window": 123456,
        "default_reasoning_level": "high",
        "supported_reasoning_levels": [
            {"effort": "low", "description": "Fast"},
            {"effort": "high", "description": "Thorough"},
            {"effort": "future-effort", "description": "New effort"}
        ],
        "unknown_future_field": true
    }))?)
}

#[test]
fn discovery_uses_remote_models_efforts_names_and_context() -> Result<()> {
    let models = DiscoveredModels::new(vec![model("new-codex-model")?])?;
    let listed = models.list(&BTreeMap::new());
    assert_eq!(listed.len(), 3);
    let future = listed
        .iter()
        .find(|model| model.id.as_str() == "new-codex-model-future-effort")
        .ok_or_else(|| eyre!("missing new reasoning variant"))?;
    assert_eq!(future.name, "New Codex Model future-effort");
    assert_eq!(future.max_context, Some(123456));
    for (id, effort) in [
        ("new-codex-model", "high"),
        ("new-codex-model-low", "low"),
        ("new-codex-model-future-effort", "future-effort"),
    ] {
        let resolved = models.resolve(&ModelId::new(id))?;
        assert_eq!(resolved.api_model, "new-codex-model");
        assert_eq!(
            resolved
                .reasoning
                .map(|reasoning| reasoning.effort)
                .as_deref(),
            Some(effort)
        );
    }
    Ok(())
}

#[test]
fn hidden_models_resolve_without_appearing_in_the_picker() -> Result<()> {
    let mut hidden = model("hidden-model")?;
    hidden.visibility = "hide".into();
    let models = DiscoveredModels::new(vec![hidden])?;
    assert!(models.list(&BTreeMap::new()).is_empty());
    assert_eq!(
        models.resolve(&ModelId::new("hidden-model-low"))?.api_model,
        "hidden-model"
    );
    Ok(())
}

#[test]
fn models_without_reasoning_are_listed_and_sent_without_reasoning() -> Result<()> {
    let mut plain = model("plain-model")?;
    plain.default_reasoning_level = None;
    plain.supported_reasoning_levels.clear();
    let models = DiscoveredModels::new(vec![plain])?;
    let listed = models.list(&BTreeMap::new());
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed.first().map(|model| model.id.as_str()),
        Some("plain-model")
    );
    assert_eq!(
        models.resolve(&ModelId::new("plain-model"))?.reasoning,
        None
    );
    Ok(())
}

#[test]
fn model_configs_override_remote_metadata() -> Result<()> {
    let models = DiscoveredModels::new(vec![model("new-model")?])?;
    let configs = BTreeMap::from([
        (
            ModelId::new("new-model"),
            ModelMetadata {
                name: Some("Custom".into()),
                max_context: Some(200000),
            },
        ),
        (
            ModelId::new("new-model-high"),
            ModelMetadata {
                name: Some("Thorough".into()),
                max_context: Some(150000),
            },
        ),
    ]);
    let listed = models
        .list(&configs)
        .into_iter()
        .map(|model| (model.id.clone(), model))
        .collect::<BTreeMap<_, _>>();
    let low = listed
        .get(&ModelId::new("new-model-low"))
        .ok_or_else(|| eyre!("missing low variant"))?;
    assert_eq!(low.name, "Custom low");
    assert_eq!(low.max_context, Some(200000));
    let high = listed
        .get(&ModelId::new("new-model-high"))
        .ok_or_else(|| eyre!("missing high variant"))?;
    assert_eq!(high.name, "Thorough");
    assert_eq!(high.max_context, Some(150000));
    Ok(())
}

#[test]
fn resolution_uses_the_longest_model_slug_and_rejects_unknown_efforts() -> Result<()> {
    let models = DiscoveredModels::new(vec![model("new-model")?, model("new-model-mini")?])?;
    assert_eq!(
        models.resolve(&ModelId::new("new-model-mini"))?.api_model,
        "new-model-mini"
    );
    assert_eq!(
        models
            .resolve(&ModelId::new("new-model-mini-low"))?
            .api_model,
        "new-model-mini"
    );
    assert!(models.resolve(&ModelId::new("unknown-model")).is_err());
    assert!(
        models
            .resolve(&ModelId::new("new-model-invalid"))
            .is_err_and(|error| error.to_string().contains("unsupported reasoning effort"))
    );
    Ok(())
}

#[test]
fn malformed_metadata_fails_and_empty_discovery_has_no_fallback() -> Result<()> {
    assert!(serde_json::from_str::<ListModelsResponse>(r#"{"data":[]}"#).is_err());
    assert!(
        serde_json::from_str::<ListModelsResponse>(r#"{"models":[{"slug":"old-format"}]}"#)
            .is_err()
    );
    let response = serde_json::from_str::<ListModelsResponse>(r#"{"models":[]}"#)?;
    let models = DiscoveredModels::new(response.models)?;
    assert!(models.list(&BTreeMap::new()).is_empty());
    assert!(models.resolve(&ModelId::new("gpt-5.5-high")).is_err());
    let duplicate = model("duplicate")?;
    assert!(DiscoveredModels::new(vec![duplicate.clone(), duplicate]).is_err());
    let mut invalid = model("invalid")?;
    invalid.default_reasoning_level = Some("unsupported".into());
    assert!(DiscoveredModels::new(vec![invalid]).is_err());
    assert!(DiscoveredModels::new(vec![model("new-model")?, model("new-model-low")?]).is_err());
    Ok(())
}
