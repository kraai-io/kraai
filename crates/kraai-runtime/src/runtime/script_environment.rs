use std::collections::BTreeMap;
use std::path::PathBuf;

use color_eyre::eyre::{Context, Result, eyre};
use kraai_types::{EnvironmentPolicy, PathPolicy, ScriptProfileSnapshot};

pub(super) fn configured_runtime_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = std::env::var_os("KRAAI_SCRIPT_RUNTIME_ROOTS")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    // Debug binaries use an ELF interpreter and shared libraries from the Nix store. Packaged
    // builds opt in through KRAAI_SCRIPT_RUNTIME_ROOTS instead of exposing the whole store by
    // default.
    if cfg!(debug_assertions) {
        let nix_store = PathBuf::from("/nix/store");
        if nix_store.is_dir() && !roots.iter().any(|root| root == &nix_store) {
            roots.push(nix_store);
        }
    }
    roots
}

pub(super) fn script_environment(
    profile: &ScriptProfileSnapshot,
) -> Result<BTreeMap<String, String>> {
    const MINIMAL: &[&str] = &["LANG", "LANGUAGE", "LC_ALL", "LC_CTYPE", "TERM", "TZ"];
    const ALLOWED: &[&str] = &[
        "COLORTERM",
        "EDITOR",
        "HOME",
        "LOGNAME",
        "PAGER",
        "SHELL",
        "USER",
        "VISUAL",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
    ];
    let mut environment = BTreeMap::new();
    match profile.environment {
        EnvironmentPolicy::Minimal => copy_environment(MINIMAL, &mut environment),
        EnvironmentPolicy::AllowList => {
            copy_environment(MINIMAL, &mut environment);
            copy_environment(ALLOWED, &mut environment);
        }
        EnvironmentPolicy::Inherit => {
            for (name, value) in std::env::vars_os() {
                let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else {
                    continue;
                };
                environment.insert(name.to_string(), value.to_string());
            }
        }
    }
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = match profile.path {
        PathPolicy::Inherit => inherited_path,
        PathPolicy::Packaged => {
            let mut entries = Vec::new();
            if let Some(directory) = std::env::current_exe()
                .ok()
                .and_then(|executable| executable.parent().map(PathBuf::from))
            {
                entries.push(directory);
            }
            entries.extend(std::env::split_paths(&inherited_path));
            std::env::join_paths(entries).context("Failed to construct packaged script PATH")?
        }
    };
    let path = path
        .into_string()
        .map_err(|_error| eyre!("Script PATH contains non-UTF-8 data"))?;
    environment.insert(String::from("PATH"), path);
    Ok(environment)
}

fn copy_environment(names: &[&str], target: &mut BTreeMap<String, String>) {
    for name in names {
        if let Ok(value) = std::env::var(name) {
            target.insert((*name).to_string(), value);
        }
    }
}
