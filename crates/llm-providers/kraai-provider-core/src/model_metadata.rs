use color_eyre::eyre::{Result, ensure, eyre};

use crate::{DynamicConfig, DynamicValue, FieldDefinition, FieldValueKind, ValidationError};

#[derive(Clone, Default)]
pub struct ConfiguredModelMetadata {
    pub name: Option<String>,
    pub max_context: Option<usize>,
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
        crate::Model {
            name: self
                .name
                .clone()
                .or(catalog.name)
                .unwrap_or_else(|| id.to_string()),
            id,
            max_context: self.max_context.or(catalog.max_context),
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
        if let Some(value) = config.get("max_context") {
            match value.as_integer() {
                Some(number) if number > 0 => {}
                Some(_) => errors.push(ValidationError {
                    field: String::from("max_context"),
                    message: String::from("Max Context must be greater than zero"),
                }),
                None => errors.push(ValidationError {
                    field: String::from("max_context"),
                    message: String::from("Max Context must be an integer"),
                }),
            }
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
        let max_context = config
            .get("max_context")
            .and_then(DynamicValue::as_integer)
            .map(usize::try_from)
            .transpose()
            .map_err(|error| eyre!("Invalid max_context: {error}"))?;
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
            max_context,
            supports_images,
            options: Vec::new(),
            remove_options: Vec::new(),
        })
    }
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "metadata tests assert after fallible parsing"
)]
mod tests {
    use super::*;

    #[test]
    fn explicit_settings_override_catalog_capabilities() {
        let catalog = crate::CatalogModelMetadata {
            name: Some("Catalog".into()),
            max_context: Some(65536),
            supports_images: Some(true),
            options: Vec::new(),
        };
        let id = kraai_types::ModelId::new("model");
        let discovered =
            ConfiguredModelMetadata::default().resolve(id.clone(), Some(catalog.clone()));
        assert!(discovered.supports_images);
        assert_eq!(discovered.max_context, Some(65536));
        let configured = ConfiguredModelMetadata {
            name: Some("Override".into()),
            max_context: Some(4096),
            supports_images: Some(false),
            options: Vec::new(),
            remove_options: Vec::new(),
        }
        .resolve(id, Some(catalog));
        assert!(!configured.supports_images);
        assert_eq!(configured.max_context, Some(4096));
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
        for max_context in [-1, 0] {
            assert_eq!(
                ConfiguredModelMetadata::validate(&DynamicConfig::from([(
                    String::from("max_context"),
                    DynamicValue::Integer(max_context),
                )])),
                vec![ValidationError {
                    field: String::from("max_context"),
                    message: String::from("Max Context must be greater than zero"),
                }],
            );
        }
    }

    #[test]
    fn parsing_keeps_existing_permissive_values_and_name_whitespace() -> Result<()> {
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
            ]))?;
            assert_eq!(metadata.name.as_deref(), expected);
            assert_eq!(metadata.max_context, Some(0));
        }
        let metadata = ConfiguredModelMetadata::from_config(&DynamicConfig::from([(
            String::from("max_context"),
            DynamicValue::Bool(false),
        )]))?;
        assert_eq!(metadata.max_context, None);
        assert!(
            ConfiguredModelMetadata::from_config(&DynamicConfig::from([(
                String::from("max_context"),
                DynamicValue::Integer(-1),
            )]))
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn configured_option_conditions_can_reference_inherited_definitions() -> Result<()> {
        let toggle =
            crate::reasoning_toggle_option(crate::ModelOptionsProtocol::OpenRouterChatCompletions)
                .ok_or_else(|| eyre!("missing toggle"))?;
        let mut budget = crate::reasoning_budget_option(
            crate::ModelOptionsProtocol::OpenRouterChatCompletions,
            Some(1),
            Some(10000),
        )
        .ok_or_else(|| eyre!("missing budget"))?;
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
        let metadata = ConfiguredModelMetadata::from_model_config(&config)?;
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
        Ok(())
    }

    #[test]
    fn duplicate_override_and_removal_ids_fail_before_merging() -> Result<()> {
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
        Ok(())
    }
}
