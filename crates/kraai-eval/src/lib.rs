#![forbid(unsafe_code)]
#![deny(clippy::all)]

mod cache;
mod cargo_dependencies;
mod command;
mod comparison;
mod execution;
mod harness;
mod manifest;
mod metrics;
mod provider_config;
mod proxy;
mod sandbox;
mod suite;
mod validation;
mod workspace;

pub use cache::{ExperimentIdentity, ResultStore, RunCoordinates};
pub use comparison::{
    ComparedRun, ComparisonResult, ComparisonSuite, PairOutcome, PairedMetric, compare,
    compare_with_cache_roots,
};
pub use harness::{HarnessProfile, ProxyKind, ResolvedHarness};
pub use manifest::{CommandSpec, NetworkPolicy, TaskManifest};
pub use metrics::{EvaluationMetrics, HarnessMetrics, ProxyMetrics, UsageMetrics};
pub use provider_config::KraaiProviderConfigRequest;
pub use proxy::ModelProxyRequest;
pub use suite::{SuiteRequest, SuiteResult, run_suite};
pub use validation::{MutationValidation, TaskValidation, validate_task};

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::command::{CommandOutcome, run_trusted};
use crate::sandbox::{ResourceLimits, rust_environment};

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub task_path: PathBuf,
    pub runner_program: PathBuf,
    pub runner_args: Vec<String>,
    pub runner_version: String,
    pub harness_name: Option<String>,
    pub model_label: Option<String>,
    pub attempt: u64,
    pub cache_dir: PathBuf,
    pub reuse_result: bool,
    pub model_proxy: Option<ModelProxyRequest>,
    pub kraai_provider_config: Option<KraaiProviderConfigRequest>,
    pub progress: Option<ProgressReporter>,
}

impl RunRequest {
    fn resolved_harness_name(&self) -> String {
        self.harness_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                self.runner_program
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| String::from("unnamed-harness"))
    }

    fn resolved_model_label(&self) -> Option<String> {
        self.model_label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProgressReporter {
    inner: Arc<Mutex<ProgressSnapshot>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub task_id: String,
    pub harness_name: String,
    pub runner_version: String,
    pub model_label: Option<String>,
    pub attempt: u64,
    pub phase: String,
}

impl ProgressReporter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> ProgressSnapshot {
        self.inner
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default()
    }

    fn initialize(
        &self,
        task_id: &str,
        harness_name: &str,
        runner_version: &str,
        model_label: Option<&str>,
        attempt: u64,
    ) {
        if let Ok(mut snapshot) = self.inner.lock() {
            *snapshot = ProgressSnapshot {
                task_id: task_id.to_owned(),
                harness_name: harness_name.to_owned(),
                runner_version: runner_version.to_owned(),
                model_label: model_label.map(str::to_owned),
                attempt,
                phase: String::from("preparing evaluation"),
            };
        }
    }

    fn set_phase(&self, phase: impl Into<String>) {
        if let Ok(mut snapshot) = self.inner.lock() {
            snapshot.phase = phase.into();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub schema_version: u32,
    pub experiment_id: String,
    pub artifact_path: PathBuf,
    pub task_id: String,
    pub harness_name: String,
    pub model_label: Option<String>,
    pub attempt: u64,
    pub runner_version: String,
    pub runner_artifact_sha256: String,
    pub task_sha256: String,
    pub grader_sha256: String,
    pub sandbox: SandboxRecord,
    pub status: RunStatus,
    pub runner: Option<ProcessRecord>,
    pub graders: Vec<ProcessRecord>,
    pub submission_sha256: Option<String>,
    pub started_at_ms: u128,
    pub completed_at_ms: u128,
    pub duration_ms: u128,
    pub model_proxy: Option<ProxyRecord>,
    pub metrics: EvaluationMetrics,
    pub controller_failure: Option<ControllerFailure>,
    pub provider_config_sha256: Option<String>,
    pub rust_environment_programs: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Passed,
    Failed,
    RunnerFailed,
    ControllerFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerFailure {
    pub phase: String,
    pub error: String,
    pub retained_work_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRecord {
    pub backend: String,
    pub network: NetworkPolicy,
    pub environment_cleared: bool,
    pub max_memory_bytes: u64,
    pub max_processes: u64,
    pub cpu_quota_percent: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRecord {
    #[serde(default)]
    pub transport_revision: u32,
    pub kind: String,
    pub upstream: String,
    pub allowed_paths: Vec<String>,
    pub max_requests: u64,
    pub credential_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessRecord {
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub output_limit_exceeded: bool,
    pub duration_ms: u128,
}

pub fn run(request: &RunRequest) -> Result<RunResult> {
    match run_resolved(request) {
        Ok(result) => Ok(result),
        Err(error) => match persist_launch_failure(request, &error) {
            Ok(path) => Err(error).wrap_err(format!(
                "evaluation launch failure recorded at {}",
                path.display()
            )),
            Err(logging_error) => Err(error).wrap_err(format!(
                "also failed to record evaluation launch failure: {logging_error:#}"
            )),
        },
    }
}

fn run_resolved(request: &RunRequest) -> Result<RunResult> {
    if request.runner_version.trim().is_empty() {
        bail!("runner version must not be empty");
    }
    let mut task = TaskManifest::load(&request.task_path)?;
    let task_dir = request.task_path.parent().unwrap_or_else(|| Path::new("."));
    task.validate(task_dir)?;
    task.resolve_source_revision(task_dir)?;

    let harness_name = request.resolved_harness_name();
    let model_label = request.resolved_model_label();
    if let Some(progress) = &request.progress {
        progress.initialize(
            &task.id,
            &harness_name,
            &request.runner_version,
            model_label.as_deref(),
            request.attempt,
        );
    }

    let runner_artifact_sha256 = cache::hash_file(&request.runner_program)?;
    let task_sha256 = task.public_digest(task_dir)?;
    let grader_sha256 = task.grader_digest(task_dir)?;
    if request.kraai_provider_config.is_some()
        && !request
            .model_proxy
            .as_ref()
            .is_some_and(ModelProxyRequest::is_codex_subscription)
    {
        bail!("Kraai provider config sanitization requires the Codex subscription proxy");
    }
    let provider_config_sha256 = request
        .kraai_provider_config
        .as_ref()
        .map(KraaiProviderConfigRequest::digest)
        .transpose()?;
    let rust_environment = task
        .runner
        .rust_toolchain
        .then(rust_environment)
        .transpose()?;
    let rust_environment_programs = rust_environment
        .as_ref()
        .map(sandbox::RustEnvironment::program_identity)
        .transpose()?;
    let identity = ExperimentIdentity {
        schema_version: 6,
        task_sha256: task_sha256.clone(),
        grader_sha256: grader_sha256.clone(),
        runner_artifact_sha256: runner_artifact_sha256.clone(),
        runner_version: request.runner_version.clone(),
        harness_name: harness_name.clone(),
        model_label: model_label.clone(),
        attempt: request.attempt,
        runner_args: request.runner_args.clone(),
        sandbox_network: if request.model_proxy.is_some() {
            NetworkPolicy::Enabled
        } else {
            task.runner.network.clone()
        },
        model_proxy: request
            .model_proxy
            .as_ref()
            .map(ModelProxyRequest::identity)
            .transpose()?,
        provider_config_sha256: provider_config_sha256.clone(),
        rust_environment_programs: rust_environment_programs.clone(),
    };
    let experiment_id = identity.digest()?;
    fs::create_dir_all(&request.cache_dir).wrap_err("create evaluation cache directory")?;
    let cache_dir = request
        .cache_dir
        .canonicalize()
        .wrap_err("canonicalize evaluation cache directory")?;
    let store = ResultStore::new(
        &cache_dir,
        &RunCoordinates {
            task_id: &task.id,
            harness_name: &harness_name,
            runner_version: &request.runner_version,
            model_label: model_label.as_deref(),
            attempt: request.attempt,
            experiment_id: &experiment_id,
        },
    );
    if let Some(result) = store.load_result()? {
        if request.reuse_result {
            return Ok(result);
        }
        bail!("experiment result already exists; use --resume or select another --start-attempt");
    }

    let run_root = cache_dir
        .join("work")
        .join(format!("{}-{}", task.id, ulid::Ulid::generate()));
    fs::create_dir_all(&run_root)?;
    let artifact_dir = store.begin()?;
    let started = Instant::now();
    let started_at_ms = unix_timestamp_ms()?;
    let result = execution::execute(execution::Execution {
        request,
        task: &task,
        task_dir,
        cache_dir: &cache_dir,
        run_root: &run_root,
        experiment_id: &experiment_id,
        identity: &identity,
        rust_environment: rust_environment.as_ref(),
        artifact_path: store.relative_dir(),
        artifact_dir: &artifact_dir,
        started,
        started_at_ms,
    });
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let phase = request.progress.as_ref().map_or_else(
                || String::from("evaluation controller"),
                |progress| progress.snapshot().phase,
            );
            let error = format!("{error:#}");
            fs::write(
                artifact_dir.join("controller-error.log"),
                format!("{error}\n"),
            )?;
            EventLog::append(artifact_dir.join("events.jsonl"))?.write(
                "controller_failed",
                serde_json::json!({"phase": phase, "error": error}),
            )?;
            RunResult {
                schema_version: 6,
                experiment_id: experiment_id.clone(),
                artifact_path: store.relative_dir().to_path_buf(),
                task_id: task.id.clone(),
                harness_name: harness_name.clone(),
                model_label: model_label.clone(),
                attempt: request.attempt,
                runner_version: request.runner_version.clone(),
                runner_artifact_sha256: runner_artifact_sha256.clone(),
                task_sha256: task_sha256.clone(),
                grader_sha256: grader_sha256.clone(),
                sandbox: SandboxRecord {
                    backend: String::from("bubblewrap+systemd-cgroup-v2"),
                    network: if request.model_proxy.is_some() {
                        NetworkPolicy::Enabled
                    } else {
                        task.runner.network.clone()
                    },
                    environment_cleared: true,
                    max_memory_bytes: task.runner.max_memory_bytes,
                    max_processes: task.runner.max_processes,
                    cpu_quota_percent: task.runner.cpu_quota_percent,
                },
                status: RunStatus::ControllerFailed,
                runner: None,
                graders: Vec::new(),
                submission_sha256: None,
                started_at_ms,
                completed_at_ms: unix_timestamp_ms()?,
                duration_ms: started.elapsed().as_millis(),
                model_proxy: None,
                metrics: EvaluationMetrics::default(),
                controller_failure: Some(ControllerFailure {
                    phase,
                    error,
                    retained_work_path: run_root.clone(),
                }),
                provider_config_sha256: provider_config_sha256.clone(),
                rust_environment_programs,
            }
        }
    };
    store.commit(
        &artifact_dir,
        &identity_json(request, &task, &result)?,
        &result,
    )?;
    if result.status != RunStatus::ControllerFailed {
        fs::remove_dir_all(&run_root).wrap_err("remove completed evaluation workspace")?;
    }
    Ok(result)
}

fn persist_launch_failure(request: &RunRequest, error: &color_eyre::Report) -> Result<PathBuf> {
    let directory = request
        .cache_dir
        .join("failures")
        .join(ulid::Ulid::generate().to_string());
    fs::create_dir_all(&directory)?;
    let path = directory.join("failure.json");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "timestamp_ms": unix_timestamp_ms()?,
            "phase": request.progress.as_ref().map(|progress| progress.snapshot().phase),
            "task_path": request.task_path,
            "runner_program": request.runner_program,
            "runner_args": request.runner_args,
            "runner_version": request.runner_version,
            "harness_name": request.harness_name,
            "model_label": request.model_label,
            "attempt": request.attempt,
            "error": format!("{error:#}"),
        }))?,
    )?;
    Ok(path)
}

fn set_progress(request: &RunRequest, phase: impl Into<String>) {
    if let Some(progress) = &request.progress {
        progress.set_phase(phase);
    }
}

fn resource_limits(task: &TaskManifest) -> ResourceLimits {
    ResourceLimits {
        max_memory_bytes: task.runner.max_memory_bytes,
        max_processes: task.runner.max_processes,
        cpu_quota_percent: task.runner.cpu_quota_percent,
    }
}

fn expand_runner_command(
    request: &RunRequest,
    task: &TaskManifest,
    workspace: &Path,
    proxy_url: Option<&str>,
    provider_config: Option<&Path>,
    provider_id: Option<&str>,
) -> Result<Vec<String>> {
    if proxy_url.is_none()
        && request
            .runner_args
            .iter()
            .any(|argument| argument.contains("{proxy_url}"))
    {
        bail!("runner arguments use {{proxy_url}} without enabling a model proxy");
    }
    if provider_config.is_none()
        && request
            .runner_args
            .iter()
            .any(|argument| argument.contains("{provider_config}"))
    {
        bail!("runner arguments use {{provider_config}} without a sanitized provider config");
    }
    if provider_id.is_none()
        && request
            .runner_args
            .iter()
            .any(|argument| argument.contains("{provider_id}"))
    {
        bail!("runner arguments use {{provider_id}} without a selected Kraai provider");
    }
    let program = request
        .runner_program
        .canonicalize()
        .wrap_err("canonicalize runner program")?;
    let workspace = workspace.to_string_lossy();
    let mut command = vec![program.to_string_lossy().into_owned()];
    let config = provider_config
        .map(|path| path.to_string_lossy())
        .unwrap_or_default();
    let replacements = [
        ("{workspace}", workspace.as_ref()),
        ("{prompt}", task.prompt.as_str()),
        ("{proxy_url}", proxy_url.unwrap_or_default()),
        ("{provider_id}", provider_id.unwrap_or_default()),
        ("{provider_config}", config.as_ref()),
    ];
    command.extend(
        request
            .runner_args
            .iter()
            .map(|arg| expand_argument(arg, &replacements)),
    );
    Ok(command)
}

fn expand_argument(argument: &str, replacements: &[(&str, &str)]) -> String {
    let mut expanded = String::new();
    let mut remaining = argument;
    while let Some(start) = remaining.find('{') {
        expanded.push_str(remaining.get(..start).unwrap_or_default());
        remaining = remaining.get(start..).unwrap_or_default();
        if let Some((placeholder, value)) = replacements
            .iter()
            .find(|(key, _)| remaining.starts_with(key))
        {
            expanded.push_str(value);
            remaining = remaining.get(placeholder.len()..).unwrap_or_default();
        } else {
            expanded.push('{');
            remaining = remaining.get(1..).unwrap_or_default();
        }
    }
    expanded.push_str(remaining);
    expanded
}

fn apply_patch(workspace: &Path, patch: &Path) -> Result<()> {
    let patch = patch.canonicalize()?;
    let outcome = run_trusted(
        &[
            String::from("git"),
            String::from("apply"),
            patch.to_string_lossy().into_owned(),
        ],
        workspace,
        Duration::from_secs(30),
    )?;
    if !outcome.success() {
        bail!(
            "hidden grader patch failed: {}",
            String::from_utf8_lossy(&outcome.stderr).trim()
        );
    }
    Ok(())
}

fn process_record(outcome: &CommandOutcome) -> ProcessRecord {
    ProcessRecord {
        command: outcome.command.clone(),
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        output_limit_exceeded: outcome.output_limit_exceeded,
        duration_ms: outcome.duration.as_millis(),
    }
}

fn write_process_logs(dir: &Path, name: &str, outcome: &CommandOutcome) -> Result<()> {
    fs::write(dir.join(format!("{name}.stdout.log")), &outcome.stdout)?;
    fs::write(dir.join(format!("{name}.stderr.log")), &outcome.stderr)?;
    Ok(())
}

fn outcome_json(outcome: &CommandOutcome) -> serde_json::Value {
    serde_json::json!({
        "command": outcome.command,
        "exit_code": outcome.exit_code,
        "timed_out": outcome.timed_out,
        "output_limit_exceeded": outcome.output_limit_exceeded,
        "duration_ms": outcome.duration.as_millis(),
    })
}

fn identity_json(
    request: &RunRequest,
    task: &TaskManifest,
    result: &RunResult,
) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "task_path": request.task_path,
        "runner_program": request.runner_program,
        "runner_args": request.runner_args,
        "task": task,
        "result": result,
    }))
}

struct EventLog {
    file: File,
}

impl EventLog {
    fn new(path: PathBuf) -> Result<Self> {
        Ok(Self {
            file: File::create(path)?,
        })
    }

    fn append(path: PathBuf) -> Result<Self> {
        Ok(Self {
            file: fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?,
        })
    }

    fn write(&mut self, event: &str, data: serde_json::Value) -> Result<()> {
        let timestamp_ms = unix_timestamp_ms()?;
        serde_json::to_writer(
            &mut self.file,
            &serde_json::json!({"timestamp_ms": timestamp_ms, "event": event, "data": data}),
        )?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        Ok(())
    }
}

fn unix_timestamp_ms() -> Result<u128> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
}

#[cfg(test)]
fn eval_assets_directory() -> PathBuf {
    std::env::var_os("KRAAI_EVAL_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals"))
}

#[cfg(test)]
mod tests {
    use super::expand_argument;

    #[test]
    fn runner_templates_preserve_placeholder_text_inside_prompts() {
        assert_eq!(
            expand_argument(
                "prefix {prompt} {provider_id} {unknown}",
                &[
                    ("{prompt}", "Keep {provider_id} literal, including 🦀."),
                    ("{provider_id}", "selected")
                ],
            ),
            "prefix Keep {provider_id} literal, including 🦀. selected {unknown}"
        );
    }
}
