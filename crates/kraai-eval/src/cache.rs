use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::proxy::ModelProxyIdentity;
use crate::temporary_directory::TemporaryDirectory;
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
    staging_root: PathBuf,
    relative_dir: PathBuf,
    final_dir: PathBuf,
}

impl ResultStore {
    pub fn new(root: &Path, staging_root: &Path, relative_dir: &Path) -> Self {
        Self {
            staging_root: staging_root.to_path_buf(),
            final_dir: root.join(relative_dir),
            relative_dir: relative_dir.to_path_buf(),
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
        load_run_result(&path).map(Some)
    }

    pub fn discard(&self) -> Result<()> {
        fs::remove_dir_all(&self.final_dir)
            .wrap_err_with(|| format!("remove {}", self.final_dir.display()))
    }

    pub(crate) fn begin(&self) -> Result<TemporaryDirectory> {
        fs::create_dir_all(&self.staging_root)?;
        let path = self.staging_root.join(ulid::Ulid::generate().to_string());
        fs::create_dir(&path)?;
        Ok(TemporaryDirectory::new(path))
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
                "evaluation result already exists at {}",
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

pub fn load_run_result(path: &Path) -> Result<RunResult> {
    let mut result = read_run_result(path)?;
    hydrate_accounting(path, &mut result)?;
    Ok(result)
}

fn read_run_result(path: &Path) -> Result<RunResult> {
    serde_json::from_slice(
        &fs::read(path).wrap_err_with(|| format!("read run result {}", path.display()))?,
    )
    .wrap_err_with(|| format!("parse run result {}", path.display()))
}

fn hydrate_accounting(path: &Path, result: &mut RunResult) -> Result<()> {
    if let Some(proxy) = result.metrics.proxy.as_mut()
        && let Some(accounting) =
            crate::load_accounting(&path.with_file_name("request-accounting.json"))?
    {
        proxy.accounting = Some(accounting);
        proxy.accounting_error = None;
    }
    Ok(())
}

pub fn path_segment(value: &str, fallback: &str) -> String {
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
    file_digest(path, true)
}

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    file_digest(path, false)
}

fn file_digest(path: &Path, prefix_length: bool) -> Result<String> {
    require_file(path)?;
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    let mut hasher = Sha256::new();
    if prefix_length {
        hasher.update(length.to_le_bytes());
    }
    let mut buffer = [0_u8; 8192];
    let mut bytes_read = 0_u64;
    loop {
        let count = match file.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        if count == 0 {
            break;
        }
        bytes_read = bytes_read.saturating_add(count as u64);
        let chunk = buffer
            .get(..count)
            .ok_or_else(|| color_eyre::eyre::eyre!("file read exceeded hash buffer"))?;
        hasher.update(chunk);
    }
    if prefix_length && bytes_read != length {
        return Ok(hash_chunks(&[fs::read(path)?]));
    }
    Ok(finish_hash(hasher))
}

pub(crate) fn read_hashed_text_file(path: &Path) -> Result<(String, String)> {
    require_file(path)?;
    let text = fs::read_to_string(path)?;
    let digest = hash_chunks(&[text.as_bytes()]);
    Ok((text, digest))
}

fn require_file(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("runner artifact is not a file: {}", path.display());
    }
    Ok(())
}

pub(crate) fn hash_chunks(chunks: &[impl AsRef<[u8]>]) -> String {
    let mut hasher = Sha256::new();
    for chunk in chunks {
        let chunk = chunk.as_ref();
        hasher.update((chunk.len() as u64).to_le_bytes());
        hasher.update(chunk);
    }
    finish_hash(hasher)
}

fn finish_hash(hasher: Sha256) -> String {
    encode_hex(&hasher.finalize())
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::eyre::ensure;

    #[test]
    fn bundle_checksum_uses_standard_sha256() -> Result<()> {
        let path = std::env::temp_dir().join(format!("kraai-sha256-{}", ulid::Ulid::generate()));
        fs::write(&path, b"abc")?;
        let digest = sha256_file(&path)?;
        let cached = hash_file(&path)?;
        fs::remove_file(&path)?;
        ensure!(digest == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        ensure!(digest != cached);
        Ok(())
    }

    #[test]
    fn streamed_file_hash_preserves_length_prefixed_identity() -> Result<()> {
        let root =
            std::env::temp_dir().join(format!("kraai-streamed-hash-{}", ulid::Ulid::generate()));
        fs::create_dir(&root)?;
        let path = root.join("artifact");
        for length in [0, 1, 8191, 8192, 8193, 32769] {
            let bytes = (0..length)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>();
            fs::write(&path, &bytes)?;
            ensure!(hash_file(&path)? == hash_chunks(&[bytes.as_slice()]));
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn hashing_supports_regular_files_with_virtual_metadata_lengths() -> Result<()> {
        let path = Path::new("/proc/sys/kernel/pid_max");
        if path.exists() {
            let bytes = fs::read(path)?;
            ensure!(hash_file(path)? == hash_chunks(&[bytes.as_slice()]));
        }
        Ok(())
    }

    #[test]
    fn hashed_text_retains_the_bytes_from_its_single_read() -> Result<()> {
        let root =
            std::env::temp_dir().join(format!("kraai-hashed-text-{}", ulid::Ulid::generate()));
        fs::create_dir(&root)?;
        let path = root.join("events");
        let original = "{\"message\":\"é🦀\"}\r\n\n";
        fs::write(&path, original)?;
        let (text, digest) = read_hashed_text_file(&path)?;
        fs::write(&path, "replacement")?;
        ensure!(text == original);
        ensure!(digest == hash_chunks(&[original.as_bytes()]));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    struct AccountingFixture {
        root: PathBuf,
        store: ResultStore,
    }

    impl AccountingFixture {
        fn new() -> Result<Self> {
            let root = std::env::temp_dir()
                .join(format!("kraai-cache-accounting-{}", ulid::Ulid::generate()));
            let store = ResultStore::new(
                &root,
                &root.join(".cache/staging"),
                Path::new("task/attempt-1"),
            );
            fs::create_dir_all(&store.final_dir)?;
            let fixture = Self { root, store };
            fs::write(
                fixture.store.final_dir.join("proxy.events.jsonl"),
                serde_json::to_vec(&serde_json::json!({
                    "method": "POST", "path": "/v1/responses", "model": "model",
                    "usage": {"total_tokens": 1_000_000, "input_tokens": 1_000_000,
                    "output_tokens": 0, "reasoning_tokens": 0, "cache_read_tokens": 0}
                }))?,
            )?;
            let proxy = crate::ProxyMetrics {
                requests: 1,
                ..Default::default()
            };
            let result: RunResult = serde_json::from_value(serde_json::json!({
                "schema_version": 6, "experiment_id": "experiment",
                "artifact_path": fixture.store.relative_dir(), "task_id": "task",
                "harness_name": "harness", "model_label": "model", "attempt": 0,
                "runner_version": "version", "runner_artifact_sha256": "runner",
                "task_sha256": "task", "grader_sha256": "grader", "status": "passed",
                "sandbox": {"backend": "test", "network": "disabled",
                "environment_cleared": true, "max_memory_bytes": 1,
                "max_processes": 1, "cpu_quota_percent": 100},
                "graders": [], "started_at_ms": 1, "completed_at_ms": 2,
                "duration_ms": 1, "metrics": {"proxy": proxy}
            }))?;
            fs::write(
                fixture.store.final_dir.join("result.json"),
                serde_json::to_vec(&result)?,
            )?;
            Ok(fixture)
        }
    }

    impl Drop for AccountingFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn cached_and_direct_loads_use_saved_accounting_and_reject_changed_events() -> Result<()> {
        let fixture = AccountingFixture::new()?;
        let path = fixture.store.final_dir.join("result.json");
        let original = fs::read(&path)?;
        let accounting = crate::analyze_requests(&path.with_file_name("proxy.events.jsonl"), 1)?;
        fs::write(
            path.with_file_name("request-accounting.json"),
            serde_json::to_vec(&accounting)?,
        )?;
        let requests = |result: &RunResult| {
            result
                .metrics
                .proxy
                .as_ref()
                .and_then(|proxy| proxy.accounting.as_ref())
                .map(|accounting| accounting.model_requests)
        };
        let cached = fixture
            .store
            .load_result()?
            .ok_or_else(|| color_eyre::eyre::eyre!("missing cached run"))?;
        ensure!(requests(&cached) == Some(1));
        ensure!(requests(&load_run_result(&path)?) == Some(1));
        ensure!(fs::read(&path)? == original);
        fs::write(path.with_file_name("proxy.events.jsonl"), "")?;
        ensure!(fixture.store.load_result().is_err() && load_run_result(&path).is_err());
        Ok(())
    }
}
