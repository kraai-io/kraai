use std::fs::{self, File};
use std::path::Path;
use std::time::{Duration, Instant};

use color_eyre::eyre::Result;

use crate::sandbox::{self, SandboxRequest, run_sandboxed};
use crate::workspace::{
    self, capture_submission, commit_fixture, materialize_base, replay_submission,
};
use crate::{
    EvaluationMetrics, EventLog, ExperimentIdentity, HarnessMetrics, KraaiProviderConfigRequest,
    NetworkPolicy, RunRequest, RunResult, RunStatus, SandboxRecord, TaskManifest, apply_patch,
    cargo_dependencies, expand_runner_command, manifest, outcome_json, process_record, proxy,
    resource_limits, set_progress, unix_timestamp_ms, write_process_logs,
};

pub(crate) struct Execution<'a> {
    pub request: &'a RunRequest,
    pub task: &'a TaskManifest,
    pub task_dir: &'a Path,
    pub cache_dir: &'a Path,
    pub run_root: &'a Path,
    pub experiment_id: &'a str,
    pub identity: &'a ExperimentIdentity,
    pub rust_environment: Option<&'a sandbox::RustEnvironment>,
    pub artifact_path: &'a Path,
    pub artifact_dir: &'a Path,
    pub started: Instant,
    pub started_at_ms: u128,
}

pub(crate) fn execute(execution: Execution<'_>) -> Result<RunResult> {
    let Execution {
        request,
        task,
        task_dir,
        cache_dir,
        run_root,
        experiment_id,
        identity,
        rust_environment,
        artifact_path,
        artifact_dir,
        started,
        started_at_ms,
    } = execution;
    let runner_artifact_sha256 = identity.runner_artifact_sha256.as_str();
    let task_sha256 = identity.task_sha256.as_str();
    let grader_sha256 = identity.grader_sha256.as_str();
    let provider_config_sha256 = identity.provider_config_sha256.as_deref();
    let rust_environment_programs = identity.rust_environment_programs.clone();
    let harness_name = identity.harness_name.as_str();
    let model_label = identity.model_label.as_deref();
    let mut events = EventLog::new(artifact_dir.join("events.jsonl"))?;
    events.write(
        "experiment_started",
        serde_json::json!({
            "experiment_id": experiment_id,
            "task_id": task.id,
            "source_revision": task.source.revision(),
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
