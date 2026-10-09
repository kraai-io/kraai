use std::collections::BTreeMap;

use color_eyre::eyre::{Result, ensure, eyre};
use kraai_types::{
    ModelOptionBinding, ModelOptionDefinition, ModelOptionKind, ModelOptionValue,
    ModelOptionValues, ModelRequestPatch, validate_model_options,
};
use serde_json::Value;

mod protocol;
pub use protocol::{
    ModelOptionsProtocol, reasoning_budget_option, reasoning_effort_option,
    reasoning_toggle_option, service_tier_options,
};

pub fn apply_model_options(
    definitions: &[ModelOptionDefinition],
    values: &ModelOptionValues,
    body: &mut Value,
) -> Result<BTreeMap<String, String>> {
    validate_model_options(definitions, values).map_err(|errors| {
        eyre!(
            errors
                .into_iter()
                .map(|error| format!("{}: {}", error.option, error.message))
                .collect::<Vec<_>>()
                .join("; ")
        )
    })?;
    let (writes, headers) = collect_model_option_effects(definitions, values)?;
    if writes.is_empty() {
        return Ok(headers);
    }
    let mut updated = body.clone();
    for (path, value) in writes {
        set_body_value(&mut updated, &path, value)?;
    }
    *body = updated;
    Ok(headers)
}

pub fn validate_model_option_effects(
    definitions: &[ModelOptionDefinition],
    values: &ModelOptionValues,
) -> Result<()> {
    collect_model_option_effects(definitions, values)?;
    Ok(())
}

fn collect_model_option_effects(
    definitions: &[ModelOptionDefinition],
    values: &ModelOptionValues,
) -> Result<(BTreeMap<String, Value>, BTreeMap<String, String>)> {
    let mut writes = BTreeMap::new();
    let mut headers = BTreeMap::new();
    for definition in definitions {
        let Some(value) = values.get(&definition.id) else {
            continue;
        };
        if let Some(binding) = &definition.binding {
            match binding {
                ModelOptionBinding::Body { path } => {
                    insert_write(&mut writes, path.clone(), serde_json::to_value(value)?)?;
                }
                ModelOptionBinding::Header { name } => {
                    insert_header(&mut headers, name, &value.to_string())?;
                }
            }
        }
        let patch = match (&definition.kind, value) {
            (ModelOptionKind::Choice { choices }, ModelOptionValue::Choice(value)) => choices
                .iter()
                .find(|choice| &choice.id == value)
                .map(|choice| &choice.patch),
            (ModelOptionKind::Boolean { enabled, disabled }, ModelOptionValue::Boolean(value)) => {
                Some(if *value { enabled } else { disabled })
            }
            _ => None,
        };
        if let Some(patch) = patch {
            collect_patch(patch, &mut writes, &mut headers)?;
        }
    }
    Ok((writes, headers))
}

fn collect_patch(
    patch: &ModelRequestPatch,
    writes: &mut BTreeMap<String, Value>,
    headers: &mut BTreeMap<String, String>,
) -> Result<()> {
    for (key, value) in &patch.body {
        flatten_body(format!("/{}", escape_pointer(key)), value, writes)?;
    }
    for (name, value) in &patch.headers {
        insert_header(headers, name, value)?;
    }
    Ok(())
}

fn flatten_body(path: String, value: &Value, writes: &mut BTreeMap<String, Value>) -> Result<()> {
    if let Some(object) = value.as_object().filter(|object| !object.is_empty()) {
        for (key, value) in object {
            flatten_body(format!("{path}/{}", escape_pointer(key)), value, writes)?;
        }
    } else {
        insert_write(writes, path, value.clone())?;
    }
    Ok(())
}

fn insert_write(writes: &mut BTreeMap<String, Value>, path: String, value: Value) -> Result<()> {
    let root = path.split('/').nth(1).unwrap_or_default();
    ensure!(
        !matches!(
            root,
            "model"
                | "messages"
                | "input"
                | "instructions"
                | "tools"
                | "stream"
                | "store"
                | "include"
                | "prompt_cache_key"
        ),
        "Model option cannot replace request field '{root}'"
    );
    for (existing, previous) in writes.iter() {
        if existing == &path {
            ensure!(
                previous == &value,
                "Selected model options conflict at {path}"
            );
            return Ok(());
        }
        ensure!(
            !path.starts_with(&format!("{existing}/"))
                && !existing.starts_with(&format!("{path}/")),
            "Selected model options have overlapping body paths {existing} and {path}"
        );
    }
    writes.insert(path, value);
    Ok(())
}

fn insert_header(headers: &mut BTreeMap<String, String>, name: &str, value: &str) -> Result<()> {
    let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())?;
    reqwest::header::HeaderValue::from_str(value)?;
    ensure!(
        !matches!(
            name.as_str(),
            "authorization" | "proxy-authorization" | "cookie" | "host" | "content-length"
        ),
        "Model option cannot replace header '{name}'"
    );
    if let Some(previous) = headers.get(name.as_str()) {
        ensure!(
            previous == value,
            "Selected model options conflict at header {name}"
        );
    }
    headers.insert(name.to_string(), value.to_owned());
    Ok(())
}

fn set_body_value(body: &mut Value, path: &str, value: Value) -> Result<()> {
    let mut segments = path
        .strip_prefix('/')
        .ok_or_else(|| eyre!("Invalid body binding '{path}'"))?
        .split('/')
        .peekable();
    let mut current = body;
    while let Some(segment) = segments.next() {
        let key = segment.replace("~1", "/").replace("~0", "~");
        let last = segments.peek().is_none();
        match current {
            Value::Object(object) => {
                if last {
                    object.insert(key, value);
                    return Ok(());
                }
                current = object
                    .entry(key)
                    .or_insert_with(|| Value::Object(Default::default()));
            }
            Value::Array(array) => {
                let index = key.parse::<usize>()?;
                ensure!(
                    index.to_string() == key,
                    "Body binding requires a canonical array index: {path}"
                );
                current = array
                    .get_mut(index)
                    .ok_or_else(|| eyre!("Body binding index is outside array: {path}"))?;
                if last {
                    *current = value;
                    return Ok(());
                }
            }
            _ => return Err(eyre!("Body binding traverses a non-object value: {path}")),
        }
    }
    Err(eyre!("Invalid body binding '{path}'"))
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests;
