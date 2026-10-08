use color_eyre::eyre::ensure;
use kraai_provider_core::{ProviderDefinition, ProviderError, ValidationError};

use super::*;

#[test]
fn custom_model_descriptors_roundtrip_through_editable_settings() -> Result<()> {
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
    )?;
    let original = config
        .models
        .first()
        .ok_or_else(|| eyre!("custom model missing"))?;
    let settings = model_settings_from_config(original.clone());
    let restored = model_config_entry_from_settings(&settings)?;
    ensure!(&restored == original);
    let document = provider_config_from_settings(&SettingsDocument {
        providers: Vec::new(),
        models: vec![settings],
    })?;
    let serialized = toml::to_string_pretty(&document)?;
    let reparsed: ProviderManagerConfig = toml::from_str(&serialized)?;
    ensure!(reparsed.models == config.models);
    let option = original
        .options
        .first()
        .ok_or_else(|| eyre!("custom option missing"))?;
    let values = kraai_types::ModelOptionValues::from([(
        "custom-depth".into(),
        kraai_types::ModelOptionValue::Choice("future-value".into()),
    )]);
    ensure!(kraai_types::validate_model_options(&original.options, &values).is_ok());
    ensure!(option.required && original.remove_options == ["service_tier"]);
    Ok(())
}

#[test]
fn validation_preserves_order_trimming_and_last_field_values() -> Result<()> {
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
    registry.register_dynamic_factory(
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
    )?;
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
    )?;
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
    let first = settings
        .providers
        .first_mut()
        .ok_or_else(|| eyre!("missing provider"))?;
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
    let actual = validate_provider_config(&config, &registry)
        .err()
        .ok_or_else(|| eyre!("invalid config passed validation"))?;
    ensure!(actual.to_string() == expected);
    ensure!(format_settings_errors(validate_settings(&settings, &registry)) == expected);
    Ok(())
}

#[tokio::test]
async fn settings_readers_preserve_missing_files_and_parse_error_context() -> Result<()> {
    let path = std::env::temp_dir().join(format!(
        "kraai-provider-config-{}.toml",
        ulid::Ulid::generate()
    ));
    let registry = ProviderRegistry::default();
    let settings = read_settings_document(&path, &registry).await?;
    let config = read_provider_config(&path, &registry).await?;
    ensure!(settings == SettingsDocument::default());
    ensure!(config.providers.is_empty() && config.models.is_empty());

    std::fs::write(&path, "[[provider]\n")?;
    let settings = read_settings_document(&path, &registry).await;
    let config = read_provider_config(&path, &registry).await;
    std::fs::remove_file(&path)?;
    let expected = format!("Failed to parse provider config {}", path.display());
    ensure!(settings.as_ref().err().map(ToString::to_string).as_deref() == Some(expected.as_str()));
    ensure!(config.as_ref().err().map(ToString::to_string).as_deref() == Some(expected.as_str()));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn settings_readers_surface_path_resolution_errors() -> Result<()> {
    let path = std::env::temp_dir().join(format!(
        "kraai-provider-config-loop-{}.toml",
        ulid::Ulid::generate()
    ));
    std::os::unix::fs::symlink(&path, &path)?;
    let registry = ProviderRegistry::default();
    let settings = read_settings_document(&path, &registry).await;
    let config = read_provider_config(&path, &registry).await;
    std::fs::remove_file(&path)?;
    ensure!(settings.is_err());
    ensure!(config.is_err());
    Ok(())
}

#[tokio::test]
async fn concurrent_settings_saves_use_independent_temporary_files() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "kraai-provider-settings-{}",
        ulid::Ulid::generate()
    ));
    std::fs::create_dir(&root)?;
    let path = root.join("providers.toml");
    let legacy_temp = path.with_extension("toml.tmp");
    std::fs::write(&legacy_temp, b"another writer")?;
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
        result?;
    }
    let persisted = std::fs::read_to_string(&path)?;
    let temporary = std::fs::read(&legacy_temp)?;
    let entries = std::fs::read_dir(&root)?.try_fold(0, |count, entry| entry.map(|_| count + 1))?;
    std::fs::remove_dir_all(&root)?;

    let expected = candidates
        .iter()
        .map(|settings| {
            Ok(toml::to_string_pretty(&provider_config_from_settings(
                settings,
            )?)?)
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(expected.contains(&persisted));
    ensure!(temporary == b"another writer");
    ensure!(entries == 2);
    Ok(())
}

#[tokio::test]
async fn provider_config_retains_the_document_that_was_validated() -> Result<()> {
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
    std::fs::write(&path, original)?;
    let replacement_path = path.clone();
    let mut registry = ProviderRegistry::default();
    registry.register_dynamic_factory(
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
    )?;

    let result = read_provider_config(&path, &registry).await;
    let persisted = std::fs::read_to_string(&path)?;
    std::fs::remove_file(&path)?;

    let actual = result?;
    let expected: ProviderManagerConfig = toml::from_str(original)?;
    ensure!(actual.providers == expected.providers);
    ensure!(actual.models == expected.models);
    ensure!(persisted == replacement);
    Ok(())
}

#[tokio::test]
async fn provider_config_and_settings_report_the_same_validation_errors() -> Result<()> {
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
    )?;
    let registry = ProviderRegistry::default();
    let config = read_provider_config(&path, &registry).await;
    let settings = read_settings_document(&path, &registry).await;
    std::fs::remove_file(&path)?;

    let expected = concat!(
        "providers[0].type_id: Unsupported provider type: unsupported\n",
        "models[0].provider_id: Model must reference an existing provider"
    );
    ensure!(
        config
            .as_ref()
            .err()
            .map(|error| error.to_string())
            .as_deref()
            == Some(expected)
    );
    ensure!(
        settings
            .as_ref()
            .err()
            .map(|error| error.to_string())
            .as_deref()
            == Some(expected)
    );
    Ok(())
}
