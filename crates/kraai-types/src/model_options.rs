use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

mod validation;
pub use validation::{
    reconcile_model_option_values, validate_model_option_values, validate_model_options,
};

pub type ModelOptionValues = BTreeMap<String, ModelOptionValue>;

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelOptionValue {
    Choice(String),
    Boolean(bool),
    Integer(i64),
}

impl std::fmt::Display for ModelOptionValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Choice(value) => formatter.write_str(value),
            Self::Boolean(value) => value.fmt(formatter),
            Self::Integer(value) => value.fmt(formatter),
        }
    }
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOptionDefinition {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_when: Option<ModelOptionCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<ModelOptionBinding>,
    #[serde(flatten)]
    pub kind: ModelOptionKind,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelOptionKind {
    Choice {
        choices: Vec<ModelOptionChoice>,
    },
    Boolean {
        #[serde(default)]
        enabled: ModelRequestPatch,
        #[serde(default)]
        disabled: ModelRequestPatch,
    },
    Integer {
        min: Option<i64>,
        max: Option<i64>,
    },
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOptionChoice {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub patch: ModelRequestPatch,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOptionCondition {
    pub option: String,
    pub value: ModelOptionValue,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelOptionBinding {
    Body { path: String },
    Header { name: String },
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRequestPatch {
    #[serde(default)]
    #[cfg_attr(feature = "typescript", ts(type = "Record<string, unknown>"))]
    pub body: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOptionError {
    pub option: String,
    pub message: String,
}

impl ModelOptionDefinition {
    pub fn is_active(&self, values: &ModelOptionValues) -> bool {
        self.active_when
            .as_ref()
            .is_none_or(|condition| values.get(&condition.option) == Some(&condition.value))
    }

    pub fn parse_value(&self, value: &str) -> Result<ModelOptionValue, String> {
        match &self.kind {
            ModelOptionKind::Choice { .. } => Ok(ModelOptionValue::Choice(value.to_owned())),
            ModelOptionKind::Boolean { .. } => value
                .parse::<bool>()
                .map(ModelOptionValue::Boolean)
                .map_err(|error| format!("{} must be true or false: {error}", self.label)),
            ModelOptionKind::Integer { .. } => value
                .parse::<i64>()
                .map(ModelOptionValue::Integer)
                .map_err(|error| format!("{} must be an integer: {error}", self.label)),
        }
    }
}

pub fn parse_model_option_assignments(
    definitions: &[ModelOptionDefinition],
    inputs: &[String],
    mut existing: ModelOptionValues,
) -> Result<ModelOptionValues, String> {
    let mut assigned = std::collections::BTreeSet::new();
    for input in inputs {
        let (id, value) = input
            .split_once('=')
            .ok_or_else(|| format!("Model option must use id=value: {input}"))?;
        if !assigned.insert(id.to_owned()) {
            return Err(format!("Model option specified more than once: {id}"));
        }
        let definition = definitions
            .iter()
            .find(|definition| definition.id == id)
            .ok_or_else(|| format!("Unknown model option: {id}"))?;
        if value.is_empty() {
            if definition.required {
                return Err(format!(
                    "{} is required and cannot be cleared",
                    definition.label
                ));
            }
            existing.remove(id);
        } else {
            existing.insert(id.to_owned(), definition.parse_value(value)?);
        }
    }
    loop {
        let selected = existing.clone();
        existing.retain(|id, _| {
            assigned.contains(id)
                || definitions
                    .iter()
                    .any(|definition| definition.id == *id && definition.is_active(&selected))
        });
        if existing.len() == selected.len() {
            break;
        }
    }
    validate_model_option_values(definitions, &existing, false).map_err(|errors| {
        errors
            .into_iter()
            .map(|error| format!("{}: {}", error.option, error.message))
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    Ok(existing)
}
