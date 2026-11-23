use std::fmt::Write;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use kraai_io::fs::{atomic_replace_in, read_optional_text};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePreferences {
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub agent_profile_id: Option<String>,
    #[serde(default)]
    pub model_options: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, kraai_types::ModelOptionValues>,
    >,
}

pub struct WorkspacePreferencesStore {
    storage_root: PathBuf,
    root: PathBuf,
}

impl WorkspacePreferencesStore {
    pub fn new(storage_root: &Path) -> Self {
        Self {
            storage_root: storage_root.to_path_buf(),
            root: storage_root.join("workspaces"),
        }
    }

    pub fn load(&self, workspace: &Path) -> Result<WorkspacePreferences> {
        let path = self.preference_path(workspace);
        let Some(content) = read_optional_text(&path)? else {
            return Ok(WorkspacePreferences::default());
        };
        serde_json::from_str(&content).with_context(|| {
            format!(
                "Failed to parse workspace preferences from {}",
                path.display()
            )
        })
    }

    pub fn save(&self, workspace: &Path, preferences: &WorkspacePreferences) -> Result<()> {
        let path = self.preference_path(workspace);
        let content = serde_json::to_string_pretty(preferences)?;
        atomic_replace_in(&self.storage_root, &path, content.as_bytes())?
            .into_result()
            .map_err(Into::into)
    }

    fn preference_path(&self, workspace: &Path) -> PathBuf {
        let bytes = workspace.as_os_str().as_encoded_bytes();
        let file_name = if bytes.len() <= (255 - ".json".len()) / 2 {
            format!("{}.json", hex_bytes(bytes))
        } else {
            format!("sha256-{}.json", hex_bytes(&Sha256::digest(bytes)))
        };
        self.root.join(file_name)
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(test)]
mod tests;
