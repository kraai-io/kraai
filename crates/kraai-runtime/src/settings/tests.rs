use kraai_provider_core::{ProviderDefinition, ProviderError, ValidationError};

use super::*;

#[test]
fn custom_model_descriptors_roundtrip_through_editable_settings() {
    let config: ProviderManagerConfig = toml::from_str(
        r#"
[[model]]
id = "private-model"
provider_id = "private-endpoint"
remove_options = ["service_tier"]
name = "Private model"

[[model.options]]
id = "custom-depth"
label = "Depth"
type = "choice"
required = true

[model.options.binding]
type = "body"
path = "/vendor/depth"

[[model.options.choices]]
id = "future-value"
label = "Deep"

[model.options.choices.patch.headers]
x-feature = "preview"

[model.options.choices.patch.body]
vendor_flags = ["one", "two"]
"#,
    )
    .unwrap();
    let original = config.models.first().expect("custom model missing");
    let settings = model_settings_from_config(original.clone());
    let restored = model_config_entry_from_settings(&settings).unwrap();
    assert!(&restored == original);
    let document = provider_config_from_settings(&SettingsDocument {
        providers: Vec::new(),
        models: vec![settings],
    })
    .unwrap();
    let serialized = toml::to_string_pretty(&document).unwrap();
    let reparsed: ProviderManagerConfig = toml::from_str(&serialized).unwrap();
    assert!(reparsed.models == config.models);
    let option = original.options.first().expect("custom option missing");
    let values = kraai_types::ModelOptionValues::from([(
        "custom-depth".into(),
        kraai_types::ModelOptionValue::Choice("future-value".into()),
    )]);
    assert!(kraai_types::validate_model_options(&original.options, &values).is_ok());
    assert!(option.required && original.remove_options == ["service_tier"]);
}

#[test]
fn validation_preserves_order_trimming_and_last_field_values() {
    let mut registry = ProviderRegistry::default();
    let validate = |config: &DynamicConfig| {
        if config.get("enabled").and_then(SettingsValue::as_bool) == Some(true) {
            Vec::new()
        } else {
            vec![ValidationError {
                field: String::from("enabled"),
                message: String::from("Expected true"),
            }]
        }
    };
    registry
        .register_dynamic_factory(
            "test",
            ProviderDefinition {
                type_id: String::from("test"),
                display_name: String::from("Test"),
                protocol_family: String::from("test"),
                description: String::new(),
                provider_fields: Vec::new(),
                model_fields: Vec::new(),
                supports_model_discovery: false,
                default_provider_id_prefix: String::from("test"),
            },
            kraai_provider_core::ProviderPricingPolicy::default(),
            |_, _| Err(ProviderError::ConfigParseError(String::from("unused"))),
            validate,
            validate,
        )
        .unwrap();
    let config: ProviderManagerConfig = toml::from_str(
        r#"
[[provider]]
id = " p "
type = " test "
enabled = false
[[provider]]
id = "p"
type = "test"
enabled = true
[[provider]]
id = "unknown"
type = " unsupported "
[[provider]]
id = "blank-type"
type = "  "
[[model]]
id = "m"
provider_id = " p "
enabled = false
[[model]]
id = "missing"
provider_id = "none"
"#,
    )
    .unwrap();
    let mut settings = SettingsDocument {
        providers: config
            .providers
            .iter()
            .cloned()
            .map(provider_settings_from_config)
            .collect(),
        models: config
            .models
            .iter()
            .cloned()
            .map(model_settings_from_config)
            .collect(),
    };
    let first = settings.providers.first_mut().expect("missing provider");
    first
        .values
        .extend([true, false].map(|value| FieldValueEntry {
            key: String::from("enabled"),
            value: SettingsValue::Bool(value),
        }));
    let expected = concat!(
        "providers[0].enabled: Expected true\n",
        "providers[1].id: Provider ID must be unique\n",
        "providers[2].type_id: Unsupported provider type: unsupported\n",
        "providers[3].type_id: Provider type is required\n",
        "models[0].enabled: Expected true\n",
        "models[1].provider_id: Model must reference an existing provider",
    );
    let actual =
        validate_provider_config(&config, &registry).expect_err("invalid config passed validation");
    assert!(actual.to_string() == expected);
    assert!(format_settings_errors(validate_settings(&settings, &registry)) == expected);
}

#[tokio::test]
async fn settings_readers_preserve_missing_files_and_parse_error_context() {
    let path = std::env::temp_dir().join(format!(
        "kraai-provider-config-{}.toml",
        ulid::Ulid::generate()
    ));
    let registry = ProviderRegistry::default();
    let settings = read_settings_document(&path, &registry).await.unwrap();
    let config = read_provider_config(&path, &registry).await.unwrap();
    assert!(settings == SettingsDocument::default());
    assert!(config.providers.is_empty() && config.models.is_empty());

    std::fs::write(&path, "[[provider]\n").unwrap();
    let settings = read_settings_document(&path, &registry).await;
    let config = read_provider_config(&path, &registry).await;
    std::fs::remove_file(&path).unwrap();
    let expected = format!("Failed to parse provider config {}", path.display());
    assert!(settings.as_ref().err().map(ToString::to_string).as_deref() == Some(expected.as_str()));
    assert!(config.as_ref().err().map(ToString::to_string).as_deref() == Some(expected.as_str()));
}

#[cfg(unix)]
#[tokio::test]
async fn settings_readers_surface_path_resolution_errors() {
    let path = std::env::temp_dir().join(format!(
        "kraai-provider-config-loop-{}.toml",
        ulid::Ulid::generate()
    ));
    std::os::unix::fs::symlink(&path, &path).unwrap();
    let registry = ProviderRegistry::default();
    let settings = read_settings_document(&path, &registry).await;
    let config = read_provider_config(&path, &registry).await;
    std::fs::remove_file(&path).unwrap();
    assert!(settings.is_err());
    assert!(config.is_err());
}

#[tokio::test]
async fn concurrent_settings_saves_use_independent_temporary_files() {
    let root = std::env::temp_dir().join(format!(
        "kraai-provider-settings-{}",
        ulid::Ulid::generate()
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("providers.toml");
    let legacy_temp = path.with_extension("toml.tmp");
    std::fs::write(&legacy_temp, b"another writer").unwrap();
    let candidates: Vec<_> = (0..8)
        .map(|index| SettingsDocument {
            providers: vec![ProviderSettings {
                id: format!("provider-{index}"),
                type_id: String::from("test"),
                values: vec![FieldValueEntry {
                    key: String::from("value"),
                    value: SettingsValue::String("x".repeat(index * 256)),
                }],
            }],
            models: Vec::new(),
        })
        .collect();
    let writes = candidates
        .iter()
        .map(|settings| write_settings_document(&path, settings));
    for result in futures::future::join_all(writes).await {
        result.unwrap();
    }
    let persisted = std::fs::read_to_string(&path).unwrap();
    let temporary = std::fs::read(&legacy_temp).unwrap();
    let entries = std::fs::read_dir(&root)
        .unwrap()
        .try_fold(0, |count, entry| entry.map(|_| count + 1))
        .unwrap();
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777
    };
    std::fs::remove_dir_all(&root).unwrap();

    let expected = candidates
        .iter()
        .map(|settings| {
            Ok(toml::to_string_pretty(&provider_config_from_settings(
                settings,
            )?)?)
        })
        .collect::<Result<Vec<_>>>()
        .unwrap();
    assert!(expected.contains(&persisted));
    assert!(temporary == b"another writer");
    assert!(entries == 2);
    #[cfg(unix)]
    assert!(mode == 0o600, "provider settings can hold API keys");
}

#[tokio::test]
async fn provider_config_retains_the_document_that_was_validated() {
    let path = std::env::temp_dir().join(format!(
        "kraai-provider-config-{}.toml",
        ulid::Ulid::generate()
    ));
    let original = r#"
[[provider]]
id = " original "
type = "test"
custom_value = "preserved"

[[model]]
id = " second "
provider_id = " original "
enabled = true

[[model]]
id = "first"
provider_id = " original "
limit = 42
"#;
    let replacement = "[[provider]]\nid = 'replacement'\ntype = 'unsupported'\n";
    std::fs::write(&path, original).unwrap();
    let replacement_path = path.clone();
    let mut registry = ProviderRegistry::default();
    registry
        .register_dynamic_factory(
            "test",
            ProviderDefinition {
                type_id: String::from("test"),
                display_name: String::from("Test"),
                protocol_family: String::from("test"),
                description: String::new(),
                provider_fields: Vec::new(),
                model_fields: Vec::new(),
                supports_model_discovery: false,
                default_provider_id_prefix: String::from("test"),
            },
            kraai_provider_core::ProviderPricingPolicy::default(),
            |_, _| Err(ProviderError::ConfigParseError(String::from("unused"))),
            move |_| {
                std::fs::write(&replacement_path, replacement)
                    .err()
                    .map(|error| ValidationError {
                        field: String::from("fixture"),
                        message: error.to_string(),
                    })
                    .into_iter()
                    .collect()
            },
            |_| Vec::new(),
        )
        .unwrap();

    let result = read_provider_config(&path, &registry).await;
    let persisted = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();

    let actual = result.unwrap();
    let expected: ProviderManagerConfig = toml::from_str(original).unwrap();
    assert!(actual.providers == expected.providers);
    assert!(actual.models == expected.models);
    assert!(persisted == replacement);
}

#[tokio::test]
async fn provider_config_and_settings_report_the_same_validation_errors() {
    let path = std::env::temp_dir().join(format!(
        "kraai-provider-config-{}.toml",
        ulid::Ulid::generate()
    ));
    std::fs::write(
        &path,
        r#"
[[provider]]
id = "provider"
type = "unsupported"

[[model]]
id = "model"
provider_id = "missing"
"#,
    )
    .unwrap();
    let registry = ProviderRegistry::default();
    let config = read_provider_config(&path, &registry).await;
    let settings = read_settings_document(&path, &registry).await;
    std::fs::remove_file(&path).unwrap();

    let expected = concat!(
        "providers[0].type_id: Unsupported provider type: unsupported\n",
        "models[0].provider_id: Model must reference an existing provider"
    );
    assert!(
        config
            .as_ref()
            .err()
            .map(|error| error.to_string())
            .as_deref()
            == Some(expected)
    );
    assert!(
        settings
            .as_ref()
            .err()
            .map(|error| error.to_string())
            .as_deref()
            == Some(expected)
    );
}
