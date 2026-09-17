use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result};
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
    Ok(kraai_persistence::agent_state_root()?
        .join("workspaces")
        .join(format!("{}.json", hex_path(&workspace_dir))))
}

fn hex_path(path: &Path) -> String {
    path.as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "regression test failures must report their setup error"
)]
mod tests {
    use super::*;

    #[test]
    fn preferences_use_session_storage_root_without_home() {
        const CHILD: &str = "KRAAI_TEST_PREFERENCES_WITHOUT_HOME";
        if std::env::var_os(CHILD).is_some() {
            assert!(std::env::var_os("HOME").is_none());
            let workspace = std::env::current_dir()
                .expect("current directory")
                .canonicalize()
                .expect("canonical workspace");
            let expected = kraai_persistence::agent_state_root()
                .expect("session storage root")
                .join("workspaces")
                .join(format!("{}.json", hex_path(&workspace)));
            assert_eq!(
                preference_path_for_current_workspace().expect("preferences path"),
                expected
            );
            return;
        }

        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "app::workspace_preferences::tests::preferences_use_session_storage_root_without_home", "--nocapture"])
            .env_remove("HOME")
            .env(CHILD, "1")
            .output()
            .expect("run regression test without HOME");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
