use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::proxy::ModelProxyIdentity;
use crate::{NetworkPolicy, RunResult};

#[derive(Debug, Serialize)]
pub struct ExperimentIdentity {
    pub schema_version: u32,
    pub task_sha256: String,
    pub grader_sha256: String,
    pub runner_artifact_sha256: String,
    pub runner_version: String,
    pub harness_name: String,
    pub model_label: Option<String>,
    pub attempt: u64,
    pub runner_args: Vec<String>,
    pub sandbox_network: NetworkPolicy,
    pub model_proxy: Option<ModelProxyIdentity>,
    pub provider_config_sha256: Option<String>,
    pub rust_environment_programs: Option<Vec<String>>,
}

impl ExperimentIdentity {
    pub fn digest(&self) -> Result<String> {
        Ok(hash_chunks(&[serde_json::to_vec(self)?]))
    }
}

pub struct ResultStore {
    root: PathBuf,
    relative_dir: PathBuf,
    final_dir: PathBuf,
}

pub struct RunCoordinates<'a> {
    pub task_id: &'a str,
    pub harness_name: &'a str,
    pub runner_version: &'a str,
    pub model_label: Option<&'a str>,
    pub attempt: u64,
    pub experiment_id: &'a str,
}

impl ResultStore {
    pub fn new(root: &Path, coordinates: &RunCoordinates<'_>) -> Self {
        let relative_dir = PathBuf::from("runs")
            .join(path_segment(coordinates.task_id, "unnamed-task"))
            .join(path_segment(coordinates.harness_name, "unnamed-harness"))
            .join(path_segment(coordinates.runner_version, "unversioned"))
            .join(path_segment(
                coordinates.model_label.unwrap_or("unlabeled-model"),
                "unlabeled-model",
            ))
            .join(format!("attempt-{}", coordinates.attempt))
            .join(coordinates.experiment_id);
        Self {
            root: root.to_path_buf(),
            final_dir: root.join(&relative_dir),
            relative_dir,
        }
    }

    pub fn relative_dir(&self) -> &Path {
        &self.relative_dir
    }

    pub fn load_result(&self) -> Result<Option<RunResult>> {
        let path = self.final_dir.join("result.json");
        if !path.exists() {
            return Ok(None);
        }
        let contents = fs::read(&path)?;
        Ok(Some(serde_json::from_slice(&contents)?))
    }

    pub fn begin(&self) -> Result<PathBuf> {
        fs::create_dir_all(self.root.join("tmp"))?;
        let path = self.root.join("tmp").join(ulid::Ulid::new().to_string());
        fs::create_dir(&path)?;
        Ok(path)
    }

    pub fn commit(
        &self,
        staging: &Path,
        manifest: &serde_json::Value,
        result: &RunResult,
    ) -> Result<()> {
        fs::write(
            staging.join("manifest.json"),
            serde_json::to_vec_pretty(manifest)?,
        )?;
        fs::write(
            staging.join("result.json"),
            serde_json::to_vec_pretty(result)?,
        )?;
        if self.final_dir.exists() {
            bail!(
                "evaluation result cache collision at {}",
                self.final_dir.display()
            );
        }
        let parent = self
            .final_dir
            .parent()
            .ok_or_else(|| color_eyre::eyre::eyre!("result directory has no parent"))?;
        fs::create_dir_all(parent)?;
        fs::rename(staging, &self.final_dir).wrap_err("atomically commit evaluation result")?;
        Ok(())
    }
}

pub(crate) fn path_segment(value: &str, fallback: &str) -> String {
    let mut segment = String::new();
    let mut previous_was_separator = false;
    for character in value.trim().chars().take(96) {
        let character = if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        {
            character
        } else {
            '-'
        };
        if character == '-' && previous_was_separator {
            continue;
        }
        previous_was_separator = character == '-';
        segment.push(character);
    }
    let segment = segment.trim_matches(['.', '-', '_']);
    if segment.is_empty() {
        fallback.to_string()
    } else {
        segment.to_string()
    }
}

pub fn hash_file(path: &Path) -> Result<String> {
    if !path.is_file() {
        bail!("runner artifact is not a file: {}", path.display());
    }
    Ok(hash_chunks(&[fs::read(path)?]))
}

pub(crate) fn hash_chunks(chunks: &[Vec<u8>]) -> String {
    let mut hasher = Sha256::new();
    for chunk in chunks {
        hasher.update((chunk.len() as u64).to_le_bytes());
        hasher.update(chunk);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_path_exposes_run_coordinates_and_sanitizes_separators() {
        let store = ResultStore::new(
            Path::new("/cache"),
            &RunCoordinates {
                task_id: "plural-files",
                harness_name: "kraai/ci",
                runner_version: "git:abc123",
                model_label: Some("gpt-5.6-sol-low"),
                attempt: 2,
                experiment_id: "deadbeef",
            },
        );
        assert_eq!(
            store.relative_dir(),
            Path::new("runs/plural-files/kraai-ci/git-abc123/gpt-5.6-sol-low/attempt-2/deadbeef")
        );
    }
}
