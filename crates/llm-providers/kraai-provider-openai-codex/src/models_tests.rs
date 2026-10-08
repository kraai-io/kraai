#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert discovered model metadata"
)]

use super::*;
use color_eyre::eyre::eyre;
use kraai_types::{ModelOptionKind, ModelOptionValue, ModelOptionValues};
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
        "service_tiers": [{"id":"future-tier","name":"New Speed","description":"A future tier"}],
        "unknown_future_field": true
    }))?)
}

#[test]
fn discovery_lists_base_models_and_requires_explicit_advertised_options() -> Result<()> {
    let models = DiscoveredModels::new(vec![model("new-model")?])?;
    let listed = models.list(&BTreeMap::new());
    assert_eq!(listed.len(), 1);
    let listed = listed.first().ok_or_else(|| eyre!("missing model"))?;
    assert_eq!(listed.id.as_str(), "new-model");
    assert_eq!(listed.name, "New Codex Model");
    assert_eq!(listed.max_context, Some(123456));
    assert!(listed.options.iter().all(|option| option.required));
    assert!(
        kraai_types::validate_model_options(&listed.options, &ModelOptionValues::new()).is_err()
    );
    let options = ModelOptionValues::from([
        (
            "reasoning_effort".into(),
            ModelOptionValue::Choice("future-effort".into()),
        ),
        (
            "processing_mode_enabled".into(),
            ModelOptionValue::Boolean(true),
        ),
        (
            "service_tier".into(),
            ModelOptionValue::Choice("future-tier".into()),
        ),
    ]);
    let mut body = json!({"model":"new-model"});
    kraai_provider_core::apply_model_options(&listed.options, &options, &mut body)?;
    assert_eq!(
        body.pointer("/reasoning/effort"),
        Some(&json!("future-effort"))
    );
    assert_eq!(body.get("service_tier"), Some(&json!("future-tier")));
    assert!(
        models
            .get(&ModelId::new("new-model-future-effort"), &BTreeMap::new())
            .is_none()
    );
    Ok(())
}

#[test]
fn processing_with_only_priority_metadata_allows_explicit_off_and_on() -> Result<()> {
    let entry: ListModelEntry = serde_json::from_value(json!({
        "slug":"priority-model", "display_name":"Priority Model", "visibility":"list",
        "service_tiers":[{"id":"priority", "name":"Fast"}],
        "default_service_tier":"priority"
    }))?;
    let models = DiscoveredModels::new(vec![entry])?;
    let model = models
        .get(&ModelId::new("priority-model"), &BTreeMap::new())
        .ok_or_else(|| eyre!("missing priority model"))?;
    let mode = model
        .options
        .iter()
        .find(|option| option.id == "service_tier")
        .ok_or_else(|| eyre!("missing tier choices"))?;
    assert!(matches!(&mode.kind, ModelOptionKind::Choice { choices }
        if choices.len() == 1 && choices.first().is_some_and(|choice| choice.id == "priority" && choice.label == "Fast")));
    let mut options = ModelOptionValues::from([(
        "processing_mode_enabled".into(),
        ModelOptionValue::Boolean(false),
    )]);
    let mut body = json!({"model":"priority-model"});
    kraai_provider_core::apply_model_options(&model.options, &options, &mut body)?;
    assert!(body.get("service_tier").is_none());
    options.insert(
        "processing_mode_enabled".into(),
        ModelOptionValue::Boolean(true),
    );
    options.insert(
        "service_tier".into(),
        ModelOptionValue::Choice("priority".into()),
    );
    kraai_provider_core::apply_model_options(&model.options, &options, &mut body)?;
    assert_eq!(body.get("service_tier"), Some(&json!("priority")));
    Ok(())
}

#[test]
fn real_model_names_never_collide_with_reasoning_choices() -> Result<()> {
    let mut plain = model("new-model-high")?;
    plain.supported_reasoning_levels.clear();
    plain.service_tiers.clear();
    let mut hidden = model("hidden")?;
    hidden.visibility = ModelVisibility::Hide;
    let models = DiscoveredModels::new(vec![model("new-model")?, plain, hidden])?;
    assert_eq!(
        models
            .list(&BTreeMap::new())
            .into_iter()
            .map(|model| model.id.to_string())
            .collect::<Vec<_>>(),
        vec!["new-model", "new-model-high"]
    );
    let plain = models
        .get(&ModelId::new("new-model-high"), &BTreeMap::new())
        .ok_or_else(|| eyre!("missing real model"))?;
    assert!(plain.options.is_empty());
    assert!(
        models
            .get(&ModelId::new("hidden"), &BTreeMap::new())
            .is_none()
    );
    Ok(())
}

#[test]
fn configured_metadata_and_options_override_discovery_and_can_remove_controls() -> Result<()> {
    let mut entry = model("vision")?;
    entry.input_modalities = vec!["text".into(), "image".into()];
    let models = DiscoveredModels::new(vec![entry])?;
    let override_option = kraai_provider_core::reasoning_effort_option(
        ModelOptionsProtocol::OpenAiResponses,
        ["custom-effort".into()],
    );
    let config = ConfiguredModelMetadata {
        name: Some("Configured".into()),
        max_context: Some(4096),
        supports_images: Some(false),
        options: vec![override_option],
        remove_options: vec!["processing_mode_enabled".into(), "service_tier".into()],
    };
    let configs = BTreeMap::from([(ModelId::new("vision"), config)]);
    let listed = models
        .get(&ModelId::new("vision"), &configs)
        .ok_or_else(|| eyre!("missing vision model"))?;
    assert_eq!(listed.name, "Configured");
    assert_eq!(listed.max_context, Some(4096));
    assert!(!listed.supports_images);
    assert_eq!(listed.options.len(), 1);
    assert!(matches!(&listed.options.first().map(|option| &option.kind),
        Some(ModelOptionKind::Choice { choices }) if choices.first().is_some_and(|choice| choice.id == "custom-effort")));
    Ok(())
}

#[test]
fn malformed_discovery_fails_without_inventing_models_or_controls() -> Result<()> {
    let unknown = DiscoveredModels::new(vec![serde_json::from_value(json!({
        "slug":"no-capability-metadata","display_name":"Unknown Capabilities","visibility":"list"
    }))?])?;
    assert!(
        unknown
            .get(&ModelId::new("no-capability-metadata"), &BTreeMap::new())
            .ok_or_else(|| eyre!("missing model without advertised controls"))?
            .options
            .is_empty()
    );
    let duplicate = model("duplicate")?;
    assert!(DiscoveredModels::new(vec![duplicate.clone(), duplicate]).is_err());
    let mut duplicate_effort = model("effort")?;
    duplicate_effort
        .supported_reasoning_levels
        .push(crate::wire::ReasoningLevel {
            effort: "low".into(),
        });
    assert!(DiscoveredModels::new(vec![duplicate_effort]).is_err());
    let mut empty_tier = model("tier")?;
    empty_tier
        .service_tiers
        .first_mut()
        .ok_or_else(|| eyre!("missing tier"))?
        .id
        .clear();
    assert!(DiscoveredModels::new(vec![empty_tier]).is_err());
    let models = DiscoveredModels::new(Vec::new())?;
    assert!(models.list(&BTreeMap::new()).is_empty());
    assert!(
        models
            .get(&ModelId::new("unknown"), &BTreeMap::new())
            .is_none()
    );
    Ok(())
}

#[test]
fn configured_models_absent_from_discovery_keep_their_identity_and_custom_options() -> Result<()> {
    let models = DiscoveredModels::default();
    let id = ModelId::new("custom-model");
    let configs = BTreeMap::from([(
        id.clone(),
        ConfiguredModelMetadata {
            name: Some("Custom".into()),
            max_context: Some(32768),
            supports_images: Some(true),
            options: vec![kraai_provider_core::reasoning_effort_option(
                ModelOptionsProtocol::OpenAiResponses,
                ["custom-effort".into()],
            )],
            remove_options: Vec::new(),
        },
    )]);
    let listed = models.list(&configs);
    let found = models
        .get(&id, &configs)
        .ok_or_else(|| eyre!("custom model missing"))?;
    assert_eq!(listed.first().map(|model| &model.id), Some(&id));
    assert_eq!(found.id, id);
    assert_eq!(found.name, "Custom");
    assert!(found.supports_images);
    assert_eq!(found.options.len(), 1);
    Ok(())
}
