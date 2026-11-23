use super::*;

#[test]
fn explicit_settings_override_catalog_capabilities() {
    let catalog = crate::CatalogModelMetadata {
        name: Some("Catalog".into()),
        max_context: Some(65536),
        max_output: Some(32768),
        supports_images: Some(true),
        options: Vec::new(),
    };
    let id = kraai_types::ModelId::new("model");
    let discovered = ConfiguredModelMetadata::default().resolve(id.clone(), Some(catalog.clone()));
    assert!(discovered.supports_images);
    assert_eq!(discovered.max_context, Some(65536));
    assert_eq!(discovered.max_output, Some(32768));
    let configured = ConfiguredModelMetadata {
        name: Some("Override".into()),
        max_context: Some(4096),
        max_output: Some(2048),
        supports_images: Some(false),
        options: Vec::new(),
        remove_options: Vec::new(),
    }
    .resolve(id, Some(catalog));
    assert!(!configured.supports_images);
    assert_eq!(configured.max_context, Some(4096));
    assert_eq!(configured.max_output, Some(2048));
    assert_eq!(configured.name, "Override");
}

#[test]
fn validation_keeps_field_order_and_typed_errors() {
    let config = DynamicConfig::from([
        (String::from("name"), DynamicValue::Bool(false)),
        (
            String::from("max_context"),
            DynamicValue::String(String::from("100")),
        ),
    ]);
    assert_eq!(
        ConfiguredModelMetadata::validate(&config),
        vec![
            ValidationError {
                field: String::from("name"),
                message: String::from("Display Name must be a string"),
            },
            ValidationError {
                field: String::from("max_context"),
                message: String::from("Max Context must be an integer"),
            },
        ],
    );
    for (field, label) in [("max_context", "Max Context"), ("max_output", "Max Output")] {
        for tokens in [-1, 0] {
            assert_eq!(
                ConfiguredModelMetadata::validate(&DynamicConfig::from([(
                    String::from(field),
                    DynamicValue::Integer(tokens),
                )])),
                vec![ValidationError {
                    field: String::from(field),
                    message: format!("{label} must be greater than zero"),
                }],
            );
        }
    }
}

#[test]
fn parsing_keeps_existing_permissive_values_and_name_whitespace() {
    for (name, expected) in [
        (
            DynamicValue::String(String::from("  Model 🦀  ")),
            Some("  Model 🦀  "),
        ),
        (DynamicValue::String(String::from(" \t\n")), None),
        (DynamicValue::Bool(false), None),
    ] {
        let metadata = ConfiguredModelMetadata::from_config(&DynamicConfig::from([
            (String::from("name"), name),
            (String::from("max_context"), DynamicValue::Integer(0)),
        ]))
        .unwrap();
        assert_eq!(metadata.name.as_deref(), expected);
        assert_eq!(metadata.max_context, Some(0));
    }
    let metadata = ConfiguredModelMetadata::from_config(&DynamicConfig::from([(
        String::from("max_context"),
        DynamicValue::Bool(false),
    )]))
    .unwrap();
    assert_eq!(metadata.max_context, None);
    assert!(
        ConfiguredModelMetadata::from_config(&DynamicConfig::from([(
            String::from("max_context"),
            DynamicValue::Integer(-1),
        )]))
        .is_err()
    );
}

#[test]
fn configured_option_conditions_can_reference_inherited_definitions() {
    let toggle =
        crate::reasoning_toggle_option(crate::ModelOptionsProtocol::OpenRouterChatCompletions)
            .expect("missing toggle");
    let mut budget = crate::reasoning_budget_option(
        crate::ModelOptionsProtocol::OpenRouterChatCompletions,
        Some(1),
        Some(10000),
    )
    .expect("missing budget");
    budget.active_when = Some(kraai_types::ModelOptionCondition {
        option: toggle.id.clone(),
        value: kraai_types::ModelOptionValue::Boolean(true),
    });
    let id = kraai_types::ModelId::new("model");
    let config = crate::ModelConfig {
        id: id.clone(),
        provider_id: kraai_types::ProviderId::new("provider"),
        options: vec![budget],
        remove_options: Vec::new(),
        config: DynamicConfig::new(),
    };
    let metadata = ConfiguredModelMetadata::from_model_config(&config).unwrap();
    let model = metadata.resolve(
        id,
        Some(crate::CatalogModelMetadata {
            options: vec![toggle],
            ..Default::default()
        }),
    );
    assert!(
        kraai_types::validate_model_option_values(&model.options, &Default::default(), false)
            .is_ok()
    );
}

#[test]
fn duplicate_override_and_removal_ids_fail_before_merging() {
    let option = crate::reasoning_effort_option(
        crate::ModelOptionsProtocol::OpenAiChatCompletions,
        ["custom".into()],
    );
    let mut config = crate::ModelConfig {
        id: kraai_types::ModelId::new("model"),
        provider_id: kraai_types::ProviderId::new("provider"),
        options: vec![option.clone(), option],
        remove_options: Vec::new(),
        config: DynamicConfig::new(),
    };
    assert!(ConfiguredModelMetadata::from_model_config(&config).is_err());
    config.options.clear();
    config.remove_options = vec!["mode".into(), "mode".into()];
    assert!(ConfiguredModelMetadata::from_model_config(&config).is_err());
    config.remove_options = vec!["".into()];
    assert!(ConfiguredModelMetadata::from_model_config(&config).is_err());
}
