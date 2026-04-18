use std::collections::HashMap;

use kraai_runtime::{ProviderDefinition, ProviderSettings};

use kraai_runtime::{FieldDefinition, FieldValueEntry, Model, SettingsValue};

pub(super) fn default_values(fields: &[FieldDefinition]) -> Vec<FieldValueEntry> {
    fields
        .iter()
        .filter_map(|field| {
            field.default_value.clone().map(|value| FieldValueEntry {
                key: field.key.clone(),
                value,
            })
        })
        .collect()
}

pub(super) fn merge_values(
    fields: &[FieldDefinition],
    existing: &[FieldValueEntry],
) -> Vec<FieldValueEntry> {
    fields
        .iter()
        .filter_map(|field| {
            existing
                .iter()
                .find(|value| value.key == field.key)
                .cloned()
                .or_else(|| {
                    field.default_value.clone().map(|value| FieldValueEntry {
                        key: field.key.clone(),
                        value,
                    })
                })
        })
        .collect()
}

pub(super) fn field_value_display(values: &[FieldValueEntry], key: &str) -> String {
    values
        .iter()
        .find(|value| value.key == key)
        .map(|value| match &value.value {
            SettingsValue::String(value) => value.clone(),
            SettingsValue::Bool(value) => {
                if *value {
                    String::from("yes")
                } else {
                    String::from("no")
                }
            }
            SettingsValue::Integer(value) => value.to_string(),
        })
        .unwrap_or_default()
}

pub(super) fn set_field_value(values: &mut Vec<FieldValueEntry>, key: &str, value: SettingsValue) {
    clear_field_value(values, key);
    values.push(FieldValueEntry {
        key: key.to_string(),
        value,
    });
}

pub(super) fn clear_field_value(values: &mut Vec<FieldValueEntry>, key: &str) {
    values.retain(|value| value.key != key);
}

pub(super) fn parse_field_input(field: &FieldDefinition, value: &str) -> Option<SettingsValue> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    match field.value_kind {
        kraai_runtime::FieldValueKind::Integer => {
            trimmed.parse::<i64>().ok().map(SettingsValue::Integer)
        }
        kraai_runtime::FieldValueKind::Boolean => {
            let normalized = trimmed.to_ascii_lowercase();
            match normalized.as_str() {
                "true" | "yes" | "1" => Some(SettingsValue::Bool(true)),
                "false" | "no" | "0" => Some(SettingsValue::Bool(false)),
                _ => None,
            }
        }
        kraai_runtime::FieldValueKind::String
        | kraai_runtime::FieldValueKind::SecretString
        | kraai_runtime::FieldValueKind::Url => Some(SettingsValue::String(trimmed.to_string())),
    }
}

pub(super) fn is_boolean_field(field: &FieldDefinition) -> bool {
    matches!(field.value_kind, kraai_runtime::FieldValueKind::Boolean)
}

pub(super) fn flatten_models_map(
    models_by_provider: &HashMap<String, Vec<Model>>,
) -> Vec<(String, Model)> {
    let mut keys: Vec<&String> = models_by_provider.keys().collect();
    keys.sort();

    let mut flattened = Vec::new();
    for provider_id in keys {
        if let Some(models) = models_by_provider.get(provider_id) {
            for model in models {
                flattened.push((provider_id.clone(), model.clone()));
            }
        }
    }

    flattened
}

pub(super) fn provider_definition_rank(definition: &ProviderDefinition) -> (u8, String, String) {
    let display = definition.display_name.to_ascii_lowercase();
    let type_id = definition.type_id.to_ascii_lowercase();
    let rank = if display == "openai" || type_id == "openai-codex" {
        0
    } else if display.contains("openai") || type_id.contains("openai") {
        1
    } else {
        2
    };
    (rank, display, type_id)
}

pub(super) fn next_provider_id(providers: &[ProviderSettings], prefix: &str) -> String {
    if !providers.iter().any(|provider| provider.id == prefix) {
        return prefix.to_string();
    }

    let mut next_index = 2usize;
    loop {
        let candidate = format!("{prefix}-{next_index}");
        if !providers.iter().any(|provider| provider.id == candidate) {
            return candidate;
        }
        next_index += 1;
    }
}
