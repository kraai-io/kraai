use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde_json::{Map, Value};

const PROFILE_OPTIONS: &[&str] = &[
    "CODEGEN_UNITS",
    "DEBUG",
    "DEBUG_ASSERTIONS",
    "INCREMENTAL",
    "LTO",
    "OPT_LEVEL",
    "OVERFLOW_CHECKS",
    "PANIC",
    "RPATH",
    "SPLIT_DEBUGINFO",
    "STRIP",
];

pub struct SelectedProfile {
    pub name: String,
    pub settings: Value,
    names: Vec<String>,
}

impl SelectedProfile {
    pub fn environment_keys(&self) -> Vec<String> {
        self.names
            .iter()
            .flat_map(|name| {
                let name = name.to_ascii_uppercase().replace('-', "_");
                PROFILE_OPTIONS.iter().flat_map(move |option| {
                    [
                        format!("CARGO_PROFILE_{name}_{option}"),
                        format!("CARGO_PROFILE_{name}_BUILD_OVERRIDE_{option}"),
                    ]
                })
            })
            .collect()
    }

    pub fn overrides(&self, environment: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let keys = self.environment_keys();
        environment
            .iter()
            .filter(|(name, _)| name.starts_with("CARGO_FEATURE_") || keys.contains(name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }
}

pub fn select(
    profiles: &Value,
    out_dir: &Path,
    cargo_profile: &str,
) -> io::Result<SelectedProfile> {
    let directory = out_dir
        .parent()
        .and_then(Path::parent)
        .filter(|path| path.file_name().is_some_and(|name| name == "build"))
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str());
    let name = match directory
        .filter(|_| !profiles.is_null())
        .unwrap_or(cargo_profile)
    {
        "debug" => "dev",
        name => name,
    };
    let mut selected = SelectedProfile {
        name: name.into(),
        settings: Value::Object(Map::new()),
        names: Vec::new(),
    };
    if profiles.is_null() {
        selected.names.push(name.into());
        selected.settings = Value::Null;
        return Ok(selected);
    }
    let mut current = Some(name.to_owned());
    while let Some(name) = current {
        if selected.names.contains(&name) {
            return Err(io::Error::other(format!(
                "profile inheritance cycle at {name}"
            )));
        }
        selected.names.push(name.clone());
        current = match profiles.get(&name).and_then(|value| value.get("inherits")) {
            Some(Value::String(parent)) => Some(parent.clone()),
            Some(_) => return Err(io::Error::other("profile inherits must be a string")),
            None => match name.as_str() {
                "dev" | "release" => None,
                "test" => Some("dev".into()),
                "bench" => Some("release".into()),
                _ => return Err(io::Error::other(format!("profile {name} has no parent"))),
            },
        };
    }
    for name in selected.names.iter().rev() {
        if let Some(settings) = profiles.get(name) {
            let mut settings = settings.clone();
            if let Some(table) = settings.as_object_mut() {
                table.remove("inherits");
            }
            merge(&mut selected.settings, &settings);
        }
    }
    Ok(selected)
}

fn merge(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target), Value::Object(source)) => {
            for (name, value) in source {
                merge(target.entry(name).or_insert(Value::Null), value);
            }
        }
        (target, source) => *target = source.clone(),
    }
}

#[cfg(test)]
#[path = "build_profile_tests.rs"]
mod tests;
