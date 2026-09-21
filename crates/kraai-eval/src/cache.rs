use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::proxy::ModelProxyIdentity;
use crate::{NetworkPolicy, PricingOptions, RunResult};

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
        self.load_result_with_pricing(None)
    }

    pub(crate) fn load_result_with_pricing(
        &self,
        pricing: Option<&PricingOptions>,
    ) -> Result<Option<RunResult>> {
        let path = self.final_dir.join("result.json");
        if !path.exists() {
            return Ok(None);
        }
        let mut result = read_run_result(&path)?;
        if let Some(pricing) = pricing
            && let Some(proxy) = result.metrics.proxy.as_mut()
        {
            let accounting = crate::analyze_requests(
                &path.with_file_name("proxy.events.jsonl"),
                proxy.requests.saturating_add(proxy.unrecorded_requests),
                pricing,
            )?;
            let output = path.with_file_name("request-accounting.json");
            let temporary =
                path.with_file_name(format!("request-accounting-{}.tmp", ulid::Ulid::generate()));
            replace_cached_accounting(
                &temporary,
                &output,
                &serde_json::to_vec_pretty(&accounting)?,
            )?;
            proxy.accounting = Some(accounting);
            proxy.accounting_error = None;
        } else {
            hydrate_accounting(&path, &mut result)?;
        }
        Ok(Some(result))
    }

    pub fn begin(&self) -> Result<PathBuf> {
        fs::create_dir_all(self.root.join("tmp"))?;
        let path = self
            .root
            .join("tmp")
            .join(ulid::Ulid::generate().to_string());
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

fn replace_cached_accounting(temporary: &Path, output: &Path, contents: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)?;
    let written = file.write_all(contents);
    drop(file);
    let result = written
        .map_err(Into::into)
        .and_then(|()| fs::rename(temporary, output).wrap_err("replace cached request accounting"));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
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
    require_file(path)?;
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    let mut hasher = Sha256::new();
    hasher.update(length.to_le_bytes());
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
    if bytes_read != length {
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
    fn cached_accounting_preserves_existing_temporary_files() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "kraai-accounting-collision-{}",
            ulid::Ulid::generate()
        ));
        fs::create_dir(&root)?;
        let temporary = root.join("accounting.tmp");
        let output = root.join("accounting.json");
        fs::write(&temporary, b"another writer")?;
        fs::write(&output, b"original")?;
        let result = replace_cached_accounting(&temporary, &output, b"replacement");
        ensure!(result.is_err());
        ensure!(fs::read(&temporary)? == b"another writer");
        ensure!(fs::read(&output)? == b"original");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn cached_accounting_cleans_failed_publication_and_replaces_successfully() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "kraai-accounting-replace-{}",
            ulid::Ulid::generate()
        ));
        fs::create_dir(&root)?;
        let temporary = root.join("accounting.tmp");
        let output = root.join("accounting.json");
        fs::create_dir(&output)?;
        fs::write(output.join("retained"), b"original")?;
        let result = replace_cached_accounting(&temporary, &output, b"replacement");
        ensure!(
            result.err().map(|error| error.to_string()).as_deref()
                == Some("replace cached request accounting"),
            "publication error lost its context"
        );
        ensure!(!temporary.exists());
        ensure!(fs::read(output.join("retained"))? == b"original");
        fs::remove_dir_all(&output)?;
        fs::write(&output, b"original")?;
        replace_cached_accounting(&temporary, &output, b"replacement")?;
        ensure!(!temporary.exists());
        ensure!(fs::read(&output)? == b"replacement");
        fs::remove_dir_all(root)?;
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
        pricing: PricingOptions,
    }

    impl AccountingFixture {
        fn new() -> Result<Self> {
            let root = std::env::temp_dir()
                .join(format!("kraai-cache-accounting-{}", ulid::Ulid::generate()));
            let store = ResultStore::new(
                &root,
                &RunCoordinates {
                    task_id: "task",
                    harness_name: "harness",
                    runner_version: "version",
                    model_label: Some("model"),
                    attempt: 0,
                    experiment_id: "experiment",
                },
            );
            fs::create_dir_all(&store.final_dir)?;
            let pricing = PricingOptions::new(Some(root.join("prices.toml")), None);
            let fixture = Self {
                root,
                store,
                pricing,
            };
            fixture.set_price(2)?;
            fs::write(
                fixture.store.final_dir.join("proxy.events.jsonl"),
                serde_json::to_vec(&serde_json::json!({
                    "method": "POST", "path": "/v1/responses", "model": "model",
                    "usage": {"total_tokens": 1_000_000, "input_tokens": 1_000_000,
                    "output_tokens": 0, "reasoning_tokens": 0, "cache_read_tokens": 0}
                }))?,
            )?;
            let accounting = crate::analyze_requests(
                &fixture.store.final_dir.join("proxy.events.jsonl"),
                1,
                &fixture.pricing,
            )?;
            let proxy = crate::ProxyMetrics {
                requests: 1,
                accounting: Some(accounting),
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

        fn set_price(&self, price: u64) -> Result<()> {
            fs::write(
                self.root.join("prices.toml"),
                format!(
                    "[[provider]]\nid = \"test\"\ntype = \"custom\"\n[[model]]\nid = \"model\"\nprovider_id = \"test\"\nprice_input = \"{price}\"\nprice_output = \"0\"\n"
                ),
            )?;
            Ok(())
        }
    }

    impl Drop for AccountingFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn estimated_cost(result: &RunResult) -> Option<kraai_types::Usd> {
        result
            .metrics
            .proxy
            .as_ref()?
            .accounting
            .as_ref()?
            .complete_cost()
    }

    #[test]
    fn cached_and_direct_loads_use_repriced_sidecars_and_reject_changed_events() -> Result<()> {
        let fixture = AccountingFixture::new()?;
        let path = fixture.store.final_dir.join("result.json");
        let original = fs::read(&path)?;
        fixture.set_price(20)?;
        let accounting = crate::analyze_requests(
            &path.with_file_name("proxy.events.jsonl"),
            1,
            &fixture.pricing,
        )?;
        fs::write(
            path.with_file_name("request-accounting.json"),
            serde_json::to_vec(&accounting)?,
        )?;
        let cached = fixture
            .store
            .load_result()?
            .ok_or_else(|| color_eyre::eyre::eyre!("missing cached run"))?;
        let direct = load_run_result(&path)?;
        ensure!(estimated_cost(&cached) == Some(kraai_types::Usd(20_000_000_000)));
        ensure!(estimated_cost(&direct) == estimated_cost(&cached));
        ensure!(fs::read(&path)? == original);
        fs::write(path.with_file_name("proxy.events.jsonl"), "")?;
        ensure!(fixture.store.load_result().is_err() && load_run_result(&path).is_err());
        Ok(())
    }

    #[test]
    fn explicit_resume_prices_saved_requests_without_changing_run_or_proxy_events() -> Result<()> {
        let fixture = AccountingFixture::new()?;
        let path = fixture.store.final_dir.join("result.json");
        let mut failed_accounting = read_run_result(&path)?;
        let proxy = failed_accounting
            .metrics
            .proxy
            .as_mut()
            .ok_or_else(|| color_eyre::eyre::eyre!("missing proxy"))?;
        proxy.accounting = None;
        proxy.accounting_error = Some(String::from("previous accounting failed"));
        fs::write(&path, serde_json::to_vec(&failed_accounting)?)?;
        let original = fs::read(&path)?;
        let events = fs::read(path.with_file_name("proxy.events.jsonl"))?;
        fixture.set_price(20)?;
        let repriced = fixture
            .store
            .load_result_with_pricing(Some(&fixture.pricing))?
            .ok_or_else(|| color_eyre::eyre::eyre!("missing cached run"))?;
        ensure!(estimated_cost(&repriced) == Some(kraai_types::Usd(20_000_000_000)));
        ensure!(estimated_cost(&load_run_result(&path)?) == estimated_cost(&repriced));
        ensure!(
            repriced
                .metrics
                .proxy
                .as_ref()
                .is_some_and(|proxy| proxy.accounting_error.is_none())
        );
        ensure!(
            load_run_result(&path)?
                .metrics
                .proxy
                .is_some_and(|proxy| proxy.accounting_error.is_none())
        );
        ensure!(
            fs::read(&path)? == original
                && fs::read(path.with_file_name("proxy.events.jsonl"))? == events
        );
        ensure!(repriced.status == crate::RunStatus::Passed && repriced.duration_ms == 1);
        Ok(())
    }

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
