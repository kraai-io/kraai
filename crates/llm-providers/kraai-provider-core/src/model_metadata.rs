use color_eyre::eyre::{Result, eyre};

use crate::{DynamicConfig, DynamicValue, FieldDefinition, FieldValueKind, ValidationError};

#[derive(Clone)]
pub struct ConfiguredModelMetadata {
    pub name: Option<String>,
    pub max_context: Option<usize>,
    pub supports_images: Option<bool>,
}

impl ConfiguredModelMetadata {
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
}
