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
    #[serde(default)]
    pub options: Vec<kraai_types::ModelOptionDefinition>,
    #[serde(default)]
    pub remove_options: Vec<String>,
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
    let Some(content) = kraai_io::fs::read_optional_async(path).await? else {
        return Ok(None);
    };
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

    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    kraai_io::fs::create_dir_all_async(directory).await?;
    kraai_io::fs::atomic_replace_async(path, toml_string.as_bytes())
        .await?
        .into_result()
        .map_err(Into::into)
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
        options: config.options,
        remove_options: config.remove_options,
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
        options: settings.options.clone(),
        remove_options: settings.remove_options.clone(),
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
mod tests;
