use color_eyre::eyre::{Result, ensure, eyre};

use crate::{DynamicConfig, DynamicValue, FieldDefinition, FieldValueKind, ValidationError};

#[derive(Clone, Default)]
pub struct ConfiguredModelMetadata {
    pub name: Option<String>,
    pub max_context: Option<usize>,
    pub max_output: Option<usize>,
    pub supports_images: Option<bool>,
    pub options: Vec<kraai_types::ModelOptionDefinition>,
    pub remove_options: Vec<String>,
}

impl ConfiguredModelMetadata {
    pub fn resolve(
        &self,
        id: kraai_types::ModelId,
        catalog: Option<crate::CatalogModelMetadata>,
    ) -> crate::Model {
        let catalog = catalog.unwrap_or_default();
        let max_context = self.max_context.or(catalog.max_context);
        crate::Model {
            name: self
                .name
                .clone()
                .or(catalog.name)
                .unwrap_or_else(|| id.to_string()),
            id,
            max_context,
            max_output: self.max_output.or(catalog.max_output),
            supports_images: self
                .supports_images
                .or(catalog.supports_images)
                .unwrap_or(false),
            options: self.merge_options(catalog.options),
        }
    }

    pub fn merge_options(
        &self,
        discovered: Vec<kraai_types::ModelOptionDefinition>,
    ) -> Vec<kraai_types::ModelOptionDefinition> {
        let mut options = discovered
            .into_iter()
            .map(|option| (option.id.clone(), option))
            .collect::<std::collections::BTreeMap<_, _>>();
        for id in &self.remove_options {
            options.remove(id);
        }
        for option in &self.options {
            options.insert(option.id.clone(), option.clone());
        }
        options.into_values().collect()
    }

    pub fn from_model_config(config: &crate::ModelConfig) -> Result<Self> {
        let mut metadata = Self::from_config(&config.config)?;
        metadata.options.clone_from(&config.options);
        metadata.remove_options.clone_from(&config.remove_options);
        for (kind, values) in [
            (
                "options",
                metadata
                    .options
                    .iter()
                    .map(|option| option.id.as_str())
                    .collect::<Vec<_>>(),
            ),
            (
                "remove_options",
                metadata
                    .remove_options
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            ),
        ] {
            let mut seen = std::collections::BTreeSet::new();
            ensure!(
                values
                    .into_iter()
                    .all(|id| !id.trim().is_empty() && seen.insert(id)),
                "Model {kind} contains an empty or duplicate option ID"
            );
        }
        Ok(metadata)
    }

    pub fn fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition {
                key: String::from("supports_images"),
                label: String::from("Image Input"),
                value_kind: FieldValueKind::Boolean,
                required: false,
                secret: false,
                help_text: Some(String::from("Whether this model accepts image input")),
                default_value: None,
            },
            FieldDefinition {
                key: String::from("name"),
                label: String::from("Display Name"),
                value_kind: FieldValueKind::String,
                required: false,
                secret: false,
                help_text: Some(String::from("Optional UI name for the model")),
                default_value: None,
            },
            FieldDefinition {
                key: String::from("max_context"),
                label: String::from("Max Context"),
                value_kind: FieldValueKind::Integer,
                required: false,
                secret: false,
                help_text: Some(String::from("Optional context limit in tokens")),
                default_value: None,
            },
            FieldDefinition {
                key: String::from("max_output"),
                label: String::from("Max Output"),
                value_kind: FieldValueKind::Integer,
                required: false,
                secret: false,
                help_text: Some(String::from(
                    "Optional response limit in tokens, replacing the catalog limit. Each response is also limited to the context left before the compaction point",
                )),
                default_value: None,
            },
        ]
    }

    pub fn validate(config: &DynamicConfig) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        if let Some(value) = config.get("name")
            && value.as_str().is_none()
        {
            errors.push(ValidationError {
                field: String::from("name"),
                message: String::from("Display Name must be a string"),
            });
        }
        for (field, label) in [("max_context", "Max Context"), ("max_output", "Max Output")] {
            let message = match config.get(field).map(DynamicValue::as_integer) {
                None | Some(Some(1..)) => continue,
                Some(Some(_)) => format!("{label} must be greater than zero"),
                Some(None) => format!("{label} must be an integer"),
            };
            errors.push(ValidationError {
                field: String::from(field),
                message,
            });
        }
        if let Some(value) = config.get("supports_images")
            && value.as_bool().is_none()
        {
            errors.push(ValidationError {
                field: String::from("supports_images"),
                message: String::from("Image Input must be a boolean"),
            });
        }
        errors
    }

    pub fn from_config(config: &DynamicConfig) -> Result<Self> {
        let name = config
            .get("name")
            .and_then(DynamicValue::as_str)
            .map(ToString::to_string)
            .filter(|value| !value.trim().is_empty());
        let tokens = |field: &str| {
            config
                .get(field)
                .and_then(DynamicValue::as_integer)
                .map(usize::try_from)
                .transpose()
                .map_err(|error| eyre!("Invalid {field}: {error}"))
        };
        let supports_images = config
            .get("supports_images")
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| eyre!("Image Input must be a boolean"))
            })
            .transpose()?;
        Ok(Self {
            name,
            max_context: tokens("max_context")?,
            max_output: tokens("max_output")?,
            supports_images,
            options: Vec::new(),
            remove_options: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests;
