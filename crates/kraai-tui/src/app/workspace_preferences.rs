use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, ContextCompat, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct WorkspacePreferences {
    #[serde(default)]
    pub(super) provider_id: Option<String>,
    #[serde(default)]
    pub(super) model_id: Option<String>,
    #[serde(default)]
    pub(super) agent_profile_id: Option<String>,
}

impl WorkspacePreferences {
    pub(super) fn load_for_current_workspace() -> Result<Self> {
        let path = preference_path_for_current_workspace()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path)?;
        serde_json::from_str(&content).with_context(|| {
            format!(
                "Failed to parse workspace preferences from {}",
                path.display()
            )
        })
    }

    pub(super) fn save_for_current_workspace(&self) -> Result<()> {
        let path = preference_path_for_current_workspace()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)?;
        let temp_path = path.with_extension("json.tmp");
        std::fs::write(&temp_path, content)?;
        std::fs::rename(&temp_path, path)?;
        Ok(())
    }
}

fn preference_path_for_current_workspace() -> Result<PathBuf> {
    let workspace_dir = std::env::current_dir()
        .and_then(|path| path.canonicalize())
        .or_else(|_| std::env::current_dir())?;
    Ok(kraai_root()?
        .join("workspaces")
        .join(format!("{}.json", hex_path(&workspace_dir))))
}

fn kraai_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("Failed to determine home directory")?;
    Ok(PathBuf::from(home).join(".kraai"))
}

fn hex_path(path: &Path) -> String {
    path.as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
