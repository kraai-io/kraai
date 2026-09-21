use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;

use color_eyre::eyre::{Result, WrapErr, eyre};
use kraai_provider_core::{
    DynamicConfig, ModelConfig, ProviderConfig, ProviderManagerConfig, ProviderRegistry,
};
use kraai_types::{ModelId, ProviderId};
use serde::{Deserialize, Serialize};

use crate::{FieldViolation, SettingsValue};

/// Editable provider settings shared across clients.
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub id: String,
    pub type_id: String,
    pub values: Vec<FieldValueEntry>,
}

/// Editable model settings shared across clients.
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSettings {
    pub id: String,
    pub provider_id: String,
    pub values: Vec<FieldValueEntry>,
}

/// Full editable settings document persisted to providers.toml.
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsDocument {
    pub providers: Vec<ProviderSettings>,
    pub models: Vec<ModelSettings>,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldValueEntry {
    pub key: String,
    pub value: SettingsValue,
}

pub(crate) async fn read_settings_document(
    path: &Path,
    registry: &ProviderRegistry,
) -> Result<SettingsDocument> {
    match load_provider_config(path).await? {
        Some(config) => settings_from_provider_config(config, registry),
        None => Ok(SettingsDocument::default()),
    }
}

pub(crate) async fn read_provider_config(
    path: &Path,
    registry: &ProviderRegistry,
) -> Result<ProviderManagerConfig> {
    let Some(config) = load_provider_config(path).await? else {
        return Ok(ProviderManagerConfig {
            providers: Vec::new(),
            models: Vec::new(),
        });
    };

    validate_provider_config(&config, registry)?;
    Ok(config)
}

async fn load_provider_config(path: &Path) -> Result<Option<ProviderManagerConfig>> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Ok(None);
    }

    let content = tokio::fs::read(path).await?;
    parse_provider_config(&content, path).map(Some)
}

fn parse_provider_config(content: &[u8], path: &Path) -> Result<ProviderManagerConfig> {
    toml::from_slice(content)
        .wrap_err_with(|| format!("Failed to parse provider config {}", path.display()))
}

pub(crate) async fn write_settings_document(
    path: &Path,
    settings: &SettingsDocument,
) -> Result<()> {
    let config = provider_config_from_settings(settings)?;
    let toml_string = toml::to_string_pretty(&config)?;

    kraai_persistence::atomic_write(path, toml_string.as_bytes()).await
}

fn settings_from_provider_config(
    config: ProviderManagerConfig,
    registry: &ProviderRegistry,
) -> Result<SettingsDocument> {
    validate_provider_config(&config, registry)?;
    let providers = config
        .providers
        .into_iter()
        .map(provider_settings_from_config)
        .collect();
    let models = config
        .models
        .into_iter()
        .map(model_settings_from_config)
        .collect();

    Ok(SettingsDocument { providers, models })
}

fn validate_provider_config(
    config: &ProviderManagerConfig,
    registry: &ProviderRegistry,
) -> Result<()> {
    let errors = validate_entries(
        config.providers.iter().map(|provider| {
            (
                provider.id.as_str(),
                provider.type_id.as_str(),
                ConfigValues::Dynamic(&provider.config),
            )
        }),
        config.models.iter().map(|model| {
            (
                model.id.as_str(),
                model.provider_id.as_str(),
                ConfigValues::Dynamic(&model.config),
            )
        }),
        registry,
    );
    if errors.is_empty() {
        Ok(())
    } else {
        Err(eyre!(format_settings_errors(errors)))
    }
}

fn provider_settings_from_config(config: ProviderConfig) -> ProviderSettings {
    ProviderSettings {
        id: config.id.to_string(),
        type_id: config.type_id,
        values: config
            .config
            .into_iter()
            .map(|(key, value)| FieldValueEntry { key, value })
            .collect(),
    }
}

fn model_settings_from_config(config: ModelConfig) -> ModelSettings {
    ModelSettings {
        id: config.id.to_string(),
        provider_id: config.provider_id.to_string(),
        values: config
            .config
            .into_iter()
            .map(|(key, value)| FieldValueEntry { key, value })
            .collect(),
    }
}

fn provider_config_from_settings(settings: &SettingsDocument) -> Result<ProviderManagerConfig> {
    let providers = settings
        .providers
        .iter()
        .map(provider_config_entry_from_settings)
        .collect::<Result<Vec<_>>>()?;
    let models = settings
        .models
        .iter()
        .map(model_config_entry_from_settings)
        .collect::<Result<Vec<_>>>()?;
    Ok(ProviderManagerConfig { providers, models })
}

fn provider_config_entry_from_settings(settings: &ProviderSettings) -> Result<ProviderConfig> {
    Ok(ProviderConfig {
        id: ProviderId::try_new(settings.id.trim().to_string())
            .map_err(|error| eyre!("invalid provider id: {error}"))?,
        type_id: settings.type_id.trim().to_string(),
        config: values_to_dynamic_config(&settings.values),
    })
}

fn model_config_entry_from_settings(settings: &ModelSettings) -> Result<ModelConfig> {
    Ok(ModelConfig {
        id: ModelId::try_new(settings.id.trim().to_string())
            .map_err(|error| eyre!("invalid model id: {error}"))?,
        provider_id: ProviderId::try_new(settings.provider_id.trim().to_string())
            .map_err(|error| eyre!("invalid provider id: {error}"))?,
        config: values_to_dynamic_config(&settings.values),
    })
}

pub(crate) fn validate_settings(
    settings: &SettingsDocument,
    registry: &ProviderRegistry,
) -> Vec<FieldViolation> {
    validate_entries(
        settings.providers.iter().map(|provider| {
            (
                provider.id.as_str(),
                provider.type_id.as_str(),
                ConfigValues::Fields(&provider.values),
            )
        }),
        settings.models.iter().map(|model| {
            (
                model.id.as_str(),
                model.provider_id.as_str(),
                ConfigValues::Fields(&model.values),
            )
        }),
        registry,
    )
}

enum ConfigValues<'a> {
    Dynamic(&'a DynamicConfig),
    Fields(&'a [FieldValueEntry]),
}

impl<'a> ConfigValues<'a> {
    fn as_config(&self) -> Cow<'a, DynamicConfig> {
        match self {
            Self::Dynamic(config) => Cow::Borrowed(config),
            Self::Fields(values) => Cow::Owned(values_to_dynamic_config(values)),
        }
    }
}

fn validate_entries<'a>(
    providers: impl IntoIterator<Item = (&'a str, &'a str, ConfigValues<'a>)>,
    models: impl IntoIterator<Item = (&'a str, &'a str, ConfigValues<'a>)>,
    registry: &ProviderRegistry,
) -> Vec<FieldViolation> {
    let mut errors = Vec::new();
    let mut provider_ids = std::collections::BTreeSet::new();
    let mut provider_types = BTreeMap::new();

    for (index, (id, type_id, values)) in providers.into_iter().enumerate() {
        let field_prefix = format!("providers[{index}]");
        let id = id.trim();
        if id.is_empty() {
            errors.push(FieldViolation {
                field: format!("{field_prefix}.id"),
                message: String::from("Provider ID is required"),
            });
        } else if !provider_ids.insert(id) {
            errors.push(FieldViolation {
                field: format!("{field_prefix}.id"),
                message: String::from("Provider ID must be unique"),
            });
        }
        let type_id = type_id.trim();
        if type_id.is_empty() {
            errors.push(FieldViolation {
                field: format!("{field_prefix}.type_id"),
                message: String::from("Provider type is required"),
            });
            continue;
        }
        if !registry.has_factory(type_id) {
            errors.push(FieldViolation {
                field: format!("{field_prefix}.type_id"),
                message: format!("Unsupported provider type: {type_id}"),
            });
            continue;
        }
        provider_types.insert(id, type_id);
        for error in registry
            .validate_provider_config(type_id, &values.as_config())
            .unwrap_or_default()
        {
            errors.push(FieldViolation {
                field: format!("{field_prefix}.{}", error.field),
                message: error.message,
            });
        }
    }

    for (index, (id, provider_id, values)) in models.into_iter().enumerate() {
        let field_prefix = format!("models[{index}]");
        if id.trim().is_empty() {
            errors.push(FieldViolation {
                field: format!("{field_prefix}.id"),
                message: String::from("Model ID is required"),
            });
        }
        if provider_id.trim().is_empty() {
            errors.push(FieldViolation {
                field: format!("{field_prefix}.provider_id"),
                message: String::from("Provider ID is required"),
            });
            continue;
        }
        let Some(provider_type) = provider_types.get(provider_id.trim()) else {
            errors.push(FieldViolation {
                field: format!("{field_prefix}.provider_id"),
                message: String::from("Model must reference an existing provider"),
            });
            continue;
        };
        for error in registry
            .validate_model_config(provider_type, &values.as_config())
            .unwrap_or_default()
        {
            errors.push(FieldViolation {
                field: format!("{field_prefix}.{}", error.field),
                message: error.message,
            });
        }
    }

    errors
}

fn values_to_dynamic_config(values: &[FieldValueEntry]) -> DynamicConfig {
    values
        .iter()
        .map(|entry| (entry.key.clone(), entry.value.clone()))
        .collect()
}

fn format_settings_errors(errors: Vec<FieldViolation>) -> String {
    errors
        .into_iter()
        .map(|error| format!("{}: {}", error.field, error.message))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre::ensure;
    use kraai_provider_core::{ProviderDefinition, ProviderError, ValidationError};

    use super::*;

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
        ensure!(
            settings.as_ref().err().map(ToString::to_string).as_deref() == Some(expected.as_str())
        );
        ensure!(
            config.as_ref().err().map(ToString::to_string).as_deref() == Some(expected.as_str())
        );
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
        let entries =
            std::fs::read_dir(&root)?.try_fold(0, |count, entry| entry.map(|_| count + 1))?;
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
}
