use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, eyre};
use persistence::agent_state_root;
use provider_core::{
    DynamicConfig, ModelConfig, ProviderConfig, ProviderManagerConfig, ProviderRegistry,
};
use types::{ModelId, ProviderId};

use crate::SettingsValue;

/// Editable provider settings shared across clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSettings {
    pub id: String,
    pub type_id: String,
    pub values: Vec<FieldValueEntry>,
}

/// Editable model settings shared across clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSettings {
    pub id: String,
    pub provider_id: String,
    pub values: Vec<FieldValueEntry>,
}

/// Full editable settings document persisted to providers.toml.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsDocument {
    pub providers: Vec<ProviderSettings>,
    pub models: Vec<ModelSettings>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldValueEntry {
    pub key: String,
    pub value: SettingsValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SettingsValidationError {
    field: String,
    message: String,
}

pub(crate) fn default_provider_config_path() -> Result<PathBuf> {
    Ok(agent_state_root()?.join("providers.toml"))
}

pub(crate) fn resolve_provider_config_path(
    provider_config_path_override: Option<PathBuf>,
) -> Result<PathBuf> {
    match provider_config_path_override {
        Some(path) => Ok(path),
        None => default_provider_config_path(),
    }
}

pub(crate) fn read_settings_document(
    path: &Path,
    registry: &ProviderRegistry,
) -> Result<SettingsDocument> {
    if !path.exists() {
        return Ok(SettingsDocument::default());
    }

    let config_slice = std::fs::read(path)?;
    let config: ProviderManagerConfig = toml::from_slice(&config_slice)
        .wrap_err_with(|| format!("Failed to parse provider config {}", path.display()))?;
    settings_from_provider_config(config, registry)
}

pub(crate) async fn write_settings_document(
    path: &Path,
    settings: &SettingsDocument,
    registry: &ProviderRegistry,
) -> Result<()> {
    let errors = validate_settings(settings, registry);
    if !errors.is_empty() {
        let message = errors
            .into_iter()
            .map(|error| format!("{}: {}", error.field, error.message))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(eyre!(message));
    }

    let config = provider_config_from_settings(settings)?;
    let toml_string = toml::to_string_pretty(&config)?;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let temp_path = path.with_extension("toml.tmp");
    tokio::fs::write(&temp_path, toml_string).await?;
    tokio::fs::rename(&temp_path, path).await?;
    Ok(())
}

fn settings_from_provider_config(
    config: ProviderManagerConfig,
    registry: &ProviderRegistry,
) -> Result<SettingsDocument> {
    let providers = config
        .providers
        .into_iter()
        .map(provider_settings_from_config)
        .collect::<Result<Vec<_>>>()?;
    let models = config
        .models
        .into_iter()
        .map(model_settings_from_config)
        .collect::<Result<Vec<_>>>()?;

    let settings = SettingsDocument { providers, models };
    let errors = validate_settings(&settings, registry);
    if errors.is_empty() {
        Ok(settings)
    } else {
        Err(eyre!(format_settings_errors(errors)))
    }
}

fn provider_settings_from_config(config: ProviderConfig) -> Result<ProviderSettings> {
    Ok(ProviderSettings {
        id: config.id.to_string(),
        type_id: config.type_id,
        values: config
            .config
            .into_iter()
            .map(|(key, value)| FieldValueEntry { key, value })
            .collect(),
    })
}

fn model_settings_from_config(config: ModelConfig) -> Result<ModelSettings> {
    Ok(ModelSettings {
        id: config.id.to_string(),
        provider_id: config.provider_id.to_string(),
        values: config
            .config
            .into_iter()
            .map(|(key, value)| FieldValueEntry { key, value })
            .collect(),
    })
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
        id: ProviderId::new(settings.id.trim().to_string()),
        type_id: settings.type_id.trim().to_string(),
        config: values_to_dynamic_config(&settings.values),
    })
}

fn model_config_entry_from_settings(settings: &ModelSettings) -> Result<ModelConfig> {
    Ok(ModelConfig {
        id: ModelId::new(settings.id.trim().to_string()),
        provider_id: ProviderId::new(settings.provider_id.trim().to_string()),
        config: values_to_dynamic_config(&settings.values),
    })
}

fn validate_settings(
    settings: &SettingsDocument,
    registry: &ProviderRegistry,
) -> Vec<SettingsValidationError> {
    let mut errors = Vec::new();
    let mut provider_ids = std::collections::BTreeSet::new();
    let mut provider_types = BTreeMap::new();

    for (index, provider) in settings.providers.iter().enumerate() {
        let field_prefix = format!("providers[{index}]");
        let id = provider.id.trim();
        if id.is_empty() {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.id"),
                message: String::from("Provider ID is required"),
            });
        } else if !provider_ids.insert(id.to_string()) {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.id"),
                message: String::from("Provider ID must be unique"),
            });
        }
        let type_id = provider.type_id.trim();
        if type_id.is_empty() {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.type_id"),
                message: String::from("Provider type is required"),
            });
            continue;
        }
        if !registry.has_factory(type_id) {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.type_id"),
                message: format!("Unsupported provider type: {type_id}"),
            });
            continue;
        }
        provider_types.insert(id.to_string(), type_id.to_string());
        for error in registry
            .validate_provider_config(type_id, &values_to_dynamic_config(&provider.values))
            .unwrap_or_default()
        {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.{}", error.field),
                message: error.message,
            });
        }
    }

    for (index, model) in settings.models.iter().enumerate() {
        let field_prefix = format!("models[{index}]");
        if model.id.trim().is_empty() {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.id"),
                message: String::from("Model ID is required"),
            });
        }
        if model.provider_id.trim().is_empty() {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.provider_id"),
                message: String::from("Provider ID is required"),
            });
            continue;
        }
        let Some(provider_type) = provider_types.get(model.provider_id.trim()) else {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.provider_id"),
                message: String::from("Model must reference an existing provider"),
            });
            continue;
        };
        for error in registry
            .validate_model_config(provider_type, &values_to_dynamic_config(&model.values))
            .unwrap_or_default()
        {
            errors.push(SettingsValidationError {
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

fn format_settings_errors(errors: Vec<SettingsValidationError>) -> String {
    errors
        .into_iter()
        .map(|error| format!("{}: {}", error.field, error.message))
        .collect::<Vec<_>>()
        .join("\n")
}
