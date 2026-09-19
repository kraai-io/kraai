use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::atomic_file::atomic_write_sync;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePreferences {
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub agent_profile_id: Option<String>,
}

pub struct WorkspacePreferencesStore {
    root: PathBuf,
}

impl WorkspacePreferencesStore {
    pub fn new(storage_root: &Path) -> Self {
        Self {
            root: storage_root.join("workspaces"),
        }
    }

    pub fn load(&self, workspace: &Path) -> Result<WorkspacePreferences> {
        let path = self.preference_path(workspace);
        if !path.exists() {
            return Ok(WorkspacePreferences::default());
        }

        let content = std::fs::read_to_string(&path)?;
        serde_json::from_str(&content).with_context(|| {
            format!(
                "Failed to parse workspace preferences from {}",
                path.display()
            )
        })
    }

    pub fn save(&self, workspace: &Path, preferences: &WorkspacePreferences) -> Result<()> {
        let path = self.preference_path(workspace);
        std::fs::create_dir_all(&self.root)?;
        let content = serde_json::to_string_pretty(preferences)?;
        atomic_write_sync(&path, content.as_bytes())
    }

    fn preference_path(&self, workspace: &Path) -> PathBuf {
        self.root.join(format!("{}.json", hex_path(workspace)))
    }
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
    use std::sync::{Arc, Barrier};

    use color_eyre::eyre::{ensure, eyre};

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
            let root = crate::agent_state_root().expect("session storage root");
            let expected = root
                .join("workspaces")
                .join(format!("{}.json", hex_path(&workspace)));
            assert_eq!(
                WorkspacePreferencesStore::new(&root).preference_path(&workspace),
                expected
            );
            return;
        }

        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "preferences::tests::preferences_use_session_storage_root_without_home",
                "--nocapture",
            ])
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

    #[test]
    fn preferences_preserve_storage_format_and_ignore_unrelated_temp_files() -> Result<()> {
        let root = test_root();
        let store = WorkspacePreferencesStore::new(&root);
        let workspace = Path::new("workspace");
        ensure!(store.load(workspace)? == WorkspacePreferences::default());
        std::fs::create_dir_all(&store.root)?;
        let path = root.join("workspaces/776f726b7370616365.json");
        std::fs::write(&path, br#"{"provider_id":"provider"}"#)?;
        let preferences = store.load(workspace)?;
        ensure!(preferences.provider_id.as_deref() == Some("provider"));
        ensure!(preferences.model_id.is_none());
        ensure!(preferences.agent_profile_id.is_none());
        let legacy_temp = path.with_extension("json.tmp");
        std::fs::write(&legacy_temp, b"another writer")?;

        store.save(workspace, &preferences)?;
        let persisted = std::fs::read_to_string(&path)?;
        let temporary = std::fs::read(&legacy_temp)?;
        std::fs::remove_dir_all(&root)?;

        ensure!(persisted == serde_json::to_string_pretty(&preferences)?);
        ensure!(temporary == b"another writer");
        Ok(())
    }

    #[test]
    fn concurrent_preference_saves_publish_complete_documents() -> Result<()> {
        let root = test_root();
        let barrier = Arc::new(Barrier::new(8));
        let mut writers = Vec::new();
        for index in 0..8 {
            let store = WorkspacePreferencesStore::new(&root);
            let barrier = Arc::clone(&barrier);
            writers.push(std::thread::spawn(move || -> Result<()> {
                let preferences = preferences_for_writer(index);
                barrier.wait();
                for _ in 0..4 {
                    store.save(Path::new("workspace"), &preferences)?;
                }
                Ok(())
            }));
        }
        for writer in writers {
            writer
                .join()
                .map_err(|_panic| eyre!("preference writer panicked"))??;
        }
        let store = WorkspacePreferencesStore::new(&root);
        let preferences = store.load(Path::new("workspace"))?;
        let entries =
            std::fs::read_dir(&store.root)?.try_fold(0, |count, entry| entry.map(|_| count + 1))?;
        std::fs::remove_dir_all(&root)?;

        ensure!((0..8).any(|index| preferences == preferences_for_writer(index)));
        ensure!(entries == 1);
        Ok(())
    }

    #[cfg(not(windows))]
    #[test]
    fn failed_preference_replacement_keeps_destination_and_removes_its_temp() -> Result<()> {
        let root = test_root();
        let store = WorkspacePreferencesStore::new(&root);
        let workspace = Path::new("workspace");
        let destination = store.preference_path(workspace);
        std::fs::create_dir_all(&destination)?;
        std::fs::write(destination.join("retained"), b"original")?;

        let result = store.save(workspace, &WorkspacePreferences::default());
        let retained = std::fs::read(destination.join("retained"))?;
        let entries =
            std::fs::read_dir(&store.root)?.try_fold(0, |count, entry| entry.map(|_| count + 1))?;
        std::fs::remove_dir_all(&root)?;

        ensure!(result.is_err());
        ensure!(retained == b"original");
        ensure!(entries == 1);
        Ok(())
    }

    fn preferences_for_writer(index: usize) -> WorkspacePreferences {
        WorkspacePreferences {
            provider_id: Some(format!("provider-{index}")),
            model_id: Some(format!("model-{}", "x".repeat(index * 256))),
            agent_profile_id: Some(format!("profile-{index}")),
        }
    }

    fn test_root() -> PathBuf {
        std::env::temp_dir().join(format!("kraai-preferences-{}", ulid::Ulid::generate()))
    }
}
