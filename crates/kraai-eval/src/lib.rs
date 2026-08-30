#![forbid(unsafe_code)]
#![deny(clippy::all)]

mod cache;
mod cargo_dependencies;
mod command;
mod manifest;
mod metrics;
mod provider_config;
mod proxy;
mod sandbox;
mod suite;
mod workspace;

pub use cache::{ExperimentIdentity, ResultStore, RunCoordinates};
pub use manifest::{CommandSpec, NetworkPolicy, TaskManifest};
pub use metrics::{EvaluationMetrics, HarnessMetrics, ProxyMetrics, UsageMetrics};
pub use provider_config::KraaiProviderConfigRequest;
pub use proxy::ModelProxyRequest;
pub use suite::{SuiteRequest, SuiteResult, run_suite};

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::command::{CommandOutcome, run_trusted};
use crate::sandbox::{ResourceLimits, SandboxRequest, run_sandboxed, rust_environment};
use crate::workspace::{capture_submission, commit_fixture, materialize_base, replay_submission};

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

    let harness_name = request
        .harness_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            request
                .runner_program
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .ok_or_else(|| color_eyre::eyre::eyre!("runner program has no file name"))?;
    let model_label = request
        .model_label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_owned);
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
        bail!("experiment result already exists; use --reuse-result or select another --attempt");
    }

    let run_root = cache_dir
        .join("work")
        .join(format!("{}-{}", task.id, ulid::Ulid::generate()));
    fs::create_dir_all(&run_root)?;
    let artifact_dir = store.begin()?;
    let started = Instant::now();
    let started_at_ms = unix_timestamp_ms()?;
    let result = execute(
        request,
        &task,
        task_dir,
        &cache_dir,
        &run_root,
        &experiment_id,
        &runner_artifact_sha256,
        &task_sha256,
        &grader_sha256,
        provider_config_sha256.as_deref(),
        rust_environment.as_ref(),
        rust_environment_programs.clone(),
        &harness_name,
        model_label.as_deref(),
        store.relative_dir(),
        &artifact_dir,
        started,
        started_at_ms,
    );
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

#[expect(
    clippy::too_many_arguments,
    reason = "execution receives resolved immutable identities"
)]
fn execute(
    request: &RunRequest,
    task: &TaskManifest,
    task_dir: &Path,
    cache_dir: &Path,
    run_root: &Path,
    experiment_id: &str,
    runner_artifact_sha256: &str,
    task_sha256: &str,
    grader_sha256: &str,
    provider_config_sha256: Option<&str>,
    rust_environment: Option<&sandbox::RustEnvironment>,
    rust_environment_programs: Option<Vec<String>>,
    harness_name: &str,
    model_label: Option<&str>,
    artifact_path: &Path,
    artifact_dir: &Path,
    started: Instant,
    started_at_ms: u128,
) -> Result<RunResult> {
    let mut events = EventLog::new(artifact_dir.join("events.jsonl"))?;
    events.write(
        "experiment_started",
        serde_json::json!({
            "experiment_id": experiment_id,
            "task_id": task.id,
            "source_revision": task.source.revision,
            "runner_artifact_sha256": runner_artifact_sha256,
            "task_sha256": task_sha256,
            "grader_sha256": grader_sha256,
        }),
    )?;

    set_progress(request, "materializing source revision");
    let base = run_root.join("base");
    events.write("source_materialization_started", serde_json::json!({}))?;
    materialize_base(task, task_dir, &base)?;
    let cargo_dependencies = if let Some(rust_environment) = rust_environment {
        set_progress(request, "fetching Rust dependencies");
        events.write("rust_dependencies_fetch_started", serde_json::json!({}))?;
        let dependencies =
            cargo_dependencies::prepare(cache_dir, &base, task_sha256, rust_environment)?;
        events.write(
            "rust_dependencies_fetch_finished",
            serde_json::json!({
                "cache_key": dependencies.key,
                "reused": dependencies.reused,
            }),
        )?;
        Some(dependencies)
    } else {
        None
    };
    set_progress(request, "starting credential proxy");
    let proxy = request
        .model_proxy
        .as_ref()
        .map(|config| config.start(artifact_dir.join("proxy.events.jsonl")))
        .transpose()?;
    let proxy_url = proxy.as_ref().map(proxy::ModelProxy::base_url);
    let provider_config_relative = if let Some(config) = &request.kraai_provider_config {
        let proxy_url = proxy_url.as_deref().ok_or_else(|| {
            color_eyre::eyre::eyre!("provider config requires an active model proxy")
        })?;
        let path = config.materialize(&base, proxy_url)?;
        commit_fixture(&base, "sanitized evaluation provider config")?;
        Some(path.strip_prefix(&base)?.to_path_buf())
    } else {
        None
    };
    events.write("source_materialization_finished", serde_json::json!({}))?;
    set_progress(request, "creating agent workspace");
    let agent_workspace = run_root.join("agent");
    workspace::copy_tree(&base, &agent_workspace)?;
    events.write("agent_workspace_created", serde_json::json!({}))?;

    let provider_config_path = provider_config_relative
        .as_ref()
        .map(|path| agent_workspace.join(path));
    let provider_id = request
        .kraai_provider_config
        .as_ref()
        .map(KraaiProviderConfigRequest::selected_provider_id)
        .transpose()?;
    let runner_command = expand_runner_command(
        request,
        task,
        &agent_workspace,
        proxy_url.as_deref(),
        provider_config_path.as_deref(),
        provider_id.as_deref(),
    )?;
    let harness_metrics_path = artifact_dir.join("harness-metrics.json");
    File::create(&harness_metrics_path)?;
    let script_executions_dir = artifact_dir.join("script-executions");
    fs::create_dir(&script_executions_dir)?;
    events.write(
        "runner_started",
        serde_json::json!({"command": runner_command}),
    )?;
    let runner_network = if proxy.is_some() {
        NetworkPolicy::Enabled
    } else {
        task.runner.network.clone()
    };
    set_progress(request, "running harness");
    let runner_outcome = run_sandboxed(SandboxRequest {
        command: runner_command.clone(),
        workspace: agent_workspace.clone(),
        timeout: Duration::from_secs(task.runner.timeout_seconds),
        network: runner_network.clone(),
        environment: proxy.as_ref().map_or_else(
            std::collections::BTreeMap::new,
            proxy::ModelProxy::environment,
        ),
        extra_programs: rust_environment
            .map(|environment| environment.programs.clone())
            .unwrap_or_default(),
        cargo_home: cargo_dependencies
            .as_ref()
            .map(|dependencies| dependencies.home.clone()),
        metrics_output: Some(harness_metrics_path.clone()),
        script_executions_dir: Some(script_executions_dir),
        resource_limits: Some(resource_limits(task)),
    })?;
    let proxy_record = proxy.as_ref().map(proxy::ModelProxy::record);
    let proxy_metrics = proxy.map(proxy::ModelProxy::finish).transpose()?;
    let harness_metrics = match HarnessMetrics::load(&harness_metrics_path) {
        Ok(metrics) => metrics,
        Err(error) => {
            events.write(
                "harness_metrics_rejected",
                serde_json::json!({"error": format!("{error:#}")}),
            )?;
            None
        }
    };
    write_process_logs(artifact_dir, "runner", &runner_outcome)?;
    events.write("runner_finished", outcome_json(&runner_outcome))?;

    set_progress(request, "capturing submission");
    let submission_path = artifact_dir.join("submission.patch");
    let submission_sha256 = capture_submission(
        &agent_workspace,
        &submission_path,
        task.max_submission_bytes,
    )?;
    events.write(
        "submission_captured",
        serde_json::json!({"sha256": submission_sha256}),
    )?;

    let mut graders = Vec::new();
    let mut passed = false;
    if runner_outcome.success() {
        set_progress(request, "preparing hidden grading workspace");
        let grading_workspace = run_root.join("grading");
        replay_submission(&base, &grading_workspace, &submission_path)?;
        events.write("grading_workspace_created", serde_json::json!({}))?;
        if let Some(patch) = &task.grader.hidden_patch {
            let patch = manifest::resolve_private_path(task_dir, patch)?;
            apply_patch(&grading_workspace, &patch)?;
            events.write("hidden_grader_applied", serde_json::json!({}))?;
        }

        passed = true;
        for (index, command) in task.grader.commands.iter().enumerate() {
            set_progress(
                request,
                format!(
                    "running grader {}/{}",
                    index + 1,
                    task.grader.commands.len()
                ),
            );
            events.write(
                "grader_started",
                serde_json::json!({"index": index, "command": command.command}),
            )?;
            let outcome = run_sandboxed(SandboxRequest {
                command: command.command.clone(),
                workspace: grading_workspace.clone(),
                timeout: Duration::from_secs(command.timeout_seconds),
                network: NetworkPolicy::Disabled,
                environment: std::collections::BTreeMap::new(),
                extra_programs: rust_environment
                    .map(|environment| environment.programs.clone())
                    .unwrap_or_default(),
                cargo_home: cargo_dependencies
                    .as_ref()
                    .map(|dependencies| dependencies.home.clone()),
                metrics_output: None,
                script_executions_dir: None,
                resource_limits: Some(resource_limits(task)),
            })?;
            write_process_logs(artifact_dir, &format!("grader-{index}"), &outcome)?;
            events.write(
                "grader_finished",
                serde_json::json!({"index": index, "outcome": outcome_json(&outcome)}),
            )?;
            passed &= outcome.success();
            graders.push(process_record(&outcome));
        }
    }

    let status = if !runner_outcome.success() {
        RunStatus::RunnerFailed
    } else if passed {
        RunStatus::Passed
    } else {
        RunStatus::Failed
    };
    let result = RunResult {
        schema_version: 6,
        experiment_id: experiment_id.to_owned(),
        artifact_path: artifact_path.to_path_buf(),
        task_id: task.id.clone(),
        harness_name: harness_name.to_owned(),
        model_label: model_label.map(str::to_owned),
        attempt: request.attempt,
        runner_version: request.runner_version.clone(),
        runner_artifact_sha256: runner_artifact_sha256.to_owned(),
        task_sha256: task_sha256.to_owned(),
        grader_sha256: grader_sha256.to_owned(),
        sandbox: SandboxRecord {
            backend: String::from("bubblewrap+systemd-cgroup-v2"),
            network: runner_network,
            environment_cleared: true,
            max_memory_bytes: task.runner.max_memory_bytes,
            max_processes: task.runner.max_processes,
            cpu_quota_percent: task.runner.cpu_quota_percent,
        },
        status,
        runner: Some(process_record(&runner_outcome)),
        graders,
        submission_sha256: Some(submission_sha256),
        started_at_ms,
        completed_at_ms: unix_timestamp_ms()?,
        duration_ms: started.elapsed().as_millis(),
        model_proxy: proxy_record,
        metrics: EvaluationMetrics {
            proxy: proxy_metrics,
            harness: harness_metrics,
        },
        controller_failure: None,
        provider_config_sha256: provider_config_sha256.map(str::to_owned),
        rust_environment_programs,
    };
    set_progress(request, format!("saving {:?} result", result.status));
    events.write("experiment_finished", serde_json::to_value(&result)?)?;
    Ok(result)
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
    command.extend(request.runner_args.iter().map(|arg| {
        arg.replace("{workspace}", &workspace)
            .replace("{prompt}", &task.prompt)
            .replace("{proxy_url}", proxy_url.unwrap_or_default())
            .replace("{provider_id}", provider_id.unwrap_or_default())
            .replace(
                "{provider_config}",
                &provider_config
                    .map(|path| path.to_string_lossy())
                    .unwrap_or_default(),
            )
    }));
    Ok(command)
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
