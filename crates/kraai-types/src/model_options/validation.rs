use std::collections::{BTreeMap, BTreeSet};

use super::{
    ModelOptionBinding, ModelOptionDefinition, ModelOptionError, ModelOptionKind, ModelOptionValue,
    ModelOptionValues,
};

pub fn validate_model_options(
    definitions: &[ModelOptionDefinition],
    values: &ModelOptionValues,
) -> Result<(), Vec<ModelOptionError>> {
    validate_model_option_values(definitions, values, true)
}

pub fn validate_model_option_values(
    definitions: &[ModelOptionDefinition],
    values: &ModelOptionValues,
    require_all: bool,
) -> Result<(), Vec<ModelOptionError>> {
    let mut errors = Vec::new();
    let mut by_id = BTreeMap::new();
    for definition in definitions {
        if definition.id.trim().is_empty()
            || by_id.insert(definition.id.as_str(), definition).is_some()
        {
            push_error(
                &mut errors,
                &definition.id,
                "Option ID is empty or duplicated",
            );
        }
        validate_definition(definition, &mut errors);
    }
    for definition in definitions {
        let mut visited = BTreeSet::new();
        let mut current = definition;
        while let Some(condition) = &current.active_when {
            if !visited.insert(current.id.as_str()) {
                push_error(
                    &mut errors,
                    &definition.id,
                    "Option conditions contain a cycle",
                );
                break;
            }
            let Some(parent) = by_id.get(condition.option.as_str()) else {
                push_error(
                    &mut errors,
                    &definition.id,
                    "Option condition references an unknown option",
                );
                break;
            };
            if let Some(message) = value_error(parent, &condition.value) {
                push_error(
                    &mut errors,
                    &definition.id,
                    &format!("Invalid condition: {message}"),
                );
                break;
            }
            current = parent;
        }
    }
    for (id, value) in values {
        let Some(definition) = by_id.get(id.as_str()) else {
            push_error(&mut errors, id, "Unknown model option");
            continue;
        };
        if let Some(message) = value_error(definition, value) {
            push_error(&mut errors, id, &message);
        }
        if !definition.is_active(values) {
            push_error(
                &mut errors,
                id,
                "Option is inactive for the selected settings",
            );
        }
    }
    if require_all {
        for definition in definitions {
            if definition.required
                && definition.is_active(values)
                && !values.contains_key(&definition.id)
            {
                push_error(
                    &mut errors,
                    &definition.id,
                    &format!("Choose {} before generating", definition.label),
                );
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_definition(definition: &ModelOptionDefinition, errors: &mut Vec<ModelOptionError>) {
    match &definition.kind {
        ModelOptionKind::Choice { choices } => {
            let mut ids = BTreeSet::new();
            if choices.is_empty()
                || choices
                    .iter()
                    .any(|choice| choice.id.trim().is_empty() || !ids.insert(&choice.id))
            {
                push_error(
                    errors,
                    &definition.id,
                    "Choices must have nonempty, unique IDs",
                );
            }
        }
        ModelOptionKind::Integer { min, max } => {
            if min.zip(*max).is_some_and(|(min, max)| min > max) {
                push_error(errors, &definition.id, "Minimum exceeds maximum");
            }
            if definition.binding.is_none() {
                push_error(
                    errors,
                    &definition.id,
                    "Integer option requires a request binding",
                );
            }
        }
        ModelOptionKind::Boolean { .. } => {}
    }
    if let Some(ModelOptionBinding::Body { path }) = &definition.binding
        && !valid_pointer(path)
    {
        push_error(
            errors,
            &definition.id,
            "Body binding requires a nonempty JSON pointer",
        );
    }
    if let Some(ModelOptionBinding::Header { name }) = &definition.binding
        && (name.is_empty() || !name.bytes().all(header_character))
    {
        push_error(errors, &definition.id, "Header binding name is invalid");
    }
}

fn valid_pointer(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'~' && !matches!(bytes.next(), Some(b'0' | b'1')) {
            return false;
        }
    }
    true
}

fn header_character(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn value_error(definition: &ModelOptionDefinition, value: &ModelOptionValue) -> Option<String> {
    match (&definition.kind, value) {
        (ModelOptionKind::Choice { choices }, ModelOptionValue::Choice(value)) => {
            (!choices.iter().any(|choice| &choice.id == value))
                .then(|| format!("Unsupported choice '{value}' for {}", definition.label))
        }
        (ModelOptionKind::Boolean { .. }, ModelOptionValue::Boolean(_)) => None,
        (ModelOptionKind::Integer { min, max }, ModelOptionValue::Integer(value)) => {
            (min.is_some_and(|min| *value < min) || max.is_some_and(|max| *value > max))
                .then(|| format!("{} is outside its supported bounds", definition.label))
        }
        _ => Some(format!("Wrong value type for {}", definition.label)),
    }
}

fn push_error(errors: &mut Vec<ModelOptionError>, option: &str, message: &str) {
    errors.push(ModelOptionError {
        option: option.to_owned(),
        message: message.to_owned(),
    });
}

#[cfg(test)]
mod tests;
