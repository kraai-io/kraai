mod scanning;
mod trajectory;

use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, ensure, eyre};
use serde_json::{Value, json};

use super::{Catalog, files};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!("kraai-viewer-{}", ulid::Ulid::generate()));
        fs::create_dir_all(&root)?;
        Ok(Self(root))
    }

    fn write(&self, path: &str, value: &Value) -> Result<PathBuf> {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().ok_or_else(|| eyre!("missing parent"))?)?;
        fs::write(&path, serde_json::to_vec(value)?)?;
        Ok(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const NATIVE: &str = "runs/task/codex/version/model/attempt-0/experiment";
const JOB: &str = "public/benchmark/codex/version/model/job";

fn set(value: &mut Value, key: &str, field: Value) -> Result<()> {
    value
        .as_object_mut()
        .ok_or_else(|| eyre!("expected fixture object"))?
        .insert(key.to_owned(), field);
    Ok(())
}

fn native(metrics: Value) -> Value {
    json!({
        "schema_version": 6, "experiment_id": "experiment", "artifact_path": NATIVE,
        "task_id": "task", "harness_name": "codex", "model_label": "model", "attempt": 0,
        "runner_version": "version", "runner_artifact_sha256": "runner", "task_sha256": "task",
        "grader_sha256": "grader", "status": "passed", "graders": [],
        "sandbox": {"backend": "test", "network": "disabled", "environment_cleared": true,
            "max_memory_bytes": 1, "max_processes": 1, "cpu_quota_percent": 100},
        "started_at_ms": 100, "completed_at_ms": 300, "duration_ms": 200, "metrics": metrics
    })
}

fn proxy() -> Value {
    json!({"requests": 3, "successful_requests": 3, "failed_requests": 0, "duration_ms": 100,
        "usage": {"total_tokens": 120, "input_tokens": 20, "cache_read_tokens": 30,
            "cache_write_tokens": 10, "output_tokens": 40, "reasoning_tokens": 20}})
}

fn accounting(directory: &Path) -> Result<()> {
    let events = b"{\"method\":\"POST\",\"path\":\"/responses\"}\n";
    fs::create_dir_all(directory)?;
    fs::write(directory.join("proxy.events.jsonl"), events)?;
    fs::write(
        directory.join("request-accounting.json"),
        serde_json::to_vec(&json!({
            "events_sha256": crate::cache::hash_chunks(&[events]),
            "model_requests": 2, "unrecorded_requests": 0,
            "context": {"samples": 2, "total_input_tokens": 60, "min_input_tokens": 25,
                "peak_input_tokens": 35, "last_input_tokens": 35, "mean_input_tokens": 30.0},
            "known_estimated_cost": 1_250_000_000, "priced_requests": 2, "unpriced_requests": 0,
            "cost_overflow": false, "requests": []
        }))?,
    )?;
    Ok(())
}

fn suite() -> Value {
    json!({"schema_version": 1, "suite_id": "suite", "artifact_path": "suites/codex/version/model/suite",
        "harness_name": "codex", "runner_version": "version", "model_label": "model",
        "started_at_ms": 100, "completed_at_ms": 300, "duration_ms": 200,
        "requested_runs": 1, "evaluated_runs": 1, "passed_runs": 1, "failed_runs": 0,
        "controller_failures": 0, "launch_failures": 0, "success_rate": 1.0,
        "wall_time_ms": {"samples": 0},
        "total_tokens": {"total": 0, "distribution": {"samples": 0}},
        "used_context_tokens": {"total": 0, "distribution": {"samples": 0}},
        "runs": [{"task_id": "task", "attempt": 0, "status": "passed",
            "experiment_id": "experiment", "artifact_path": NATIVE, "duration_ms": 200}]
    })
}

fn harbor_result() -> Value {
    json!({"id": "trial", "task_name": "org/task", "trial_name": "task__trial",
        "agent_info": {"name": "codex", "version": "0.1"},
        "config": {"agent": {"model_name": "model"}},
        "agent_result": {"n_input_tokens": 100, "n_cache_tokens": 40,
            "n_output_tokens": 20, "cost_usd": 0.25},
        "verifier_result": {"rewards": {"reward": 1.0}},
        "started_at": "2026-09-20T10:00:00Z", "finished_at": "2026-09-20T10:00:02Z"})
}

#[test]
fn discovers_saved_codex_and_counts_reused_suite_runs_once() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(
        &format!("{NATIVE}/result.json"),
        &native(json!({"proxy": proxy(),
        "harness": {"schema_version": 1, "turns": 2, "final_context_tokens": 99}})),
    )?;
    accounting(&fixture.0.join(NATIVE))?;
    for name in ["first", "second"] {
        fixture.write(
            &format!("suites/codex/version/model/{name}/summary.json"),
            &suite(),
        )?;
    }
    let catalog = Catalog::load(&fixture.0)?;
    ensure!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
    ensure!(catalog.versions.len() == 1 && catalog.attempts.len() == 1);
    let attempt = catalog
        .attempts
        .first()
        .ok_or_else(|| eyre!("missing attempt"))?;
    let metrics = &attempt.metrics;
    ensure!(metrics.input_tokens == Some(60));
    ensure!(metrics.cached_input_tokens == Some(30) && metrics.uncached_input_tokens == Some(20));
    ensure!(metrics.output_tokens == Some(60) && metrics.reasoning_tokens == Some(20));
    ensure!(metrics.final_context_tokens == Some(35) && metrics.turns == Some(2));
    ensure!(metrics.requests == Some(2) && metrics.duration_ms == Some(200));
    ensure!(metrics.cost_usd == Some(1.25));
    ensure!(
        catalog
            .versions
            .first()
            .is_some_and(|version| version.harness == "codex")
    );
    Ok(())
}

#[test]
fn unavailable_measurements_remain_null_and_corrupt_results_remain_visible() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(&format!("{NATIVE}/result.json"), &native(json!({})))?;
    let broken = fixture.write(
        "runs/broken/kraai/v/m/attempt-1/id/result.json",
        &Value::Null,
    )?;
    fs::write(broken, b"{partial")?;
    fs::create_dir_all(fixture.0.join("runs/missing/kraai/v/m/attempt-2/id"))?;
    fixture.write("dependencies/ignored/result.json", &native(json!({})))?;
    let catalog = Catalog::load(&fixture.0)?;
    ensure!(catalog.attempts.len() == 3);
    let saved = catalog
        .attempts
        .iter()
        .find(|attempt| attempt.task == "task")
        .ok_or_else(|| eyre!("missing saved attempt"))?;
    let metrics = serde_json::to_value(&saved.metrics)?;
    ensure!(
        metrics.get("input_tokens").is_some_and(Value::is_null)
            && metrics.get("cost_usd").is_some_and(Value::is_null)
    );
    ensure!(
        catalog
            .attempts
            .iter()
            .filter(|attempt| attempt.status == "interrupted")
            .count()
            == 2
    );
    ensure!(catalog.warnings.len() == 2);
    Ok(())
}

#[test]
fn harbor_preserves_unknown_metrics_and_retains_unfinished_trials() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(
        &format!("{JOB}/lock.json"),
        &json!({"trials": [{}, {}, {}]}),
    )?;
    fixture.write(&format!("{JOB}/task__trial/result.json"), &harbor_result())?;
    fixture.write(
        &format!("{JOB}/pending__trial/lock.json"),
        &json!({"task": {"name": "org/pending"}}),
    )?;
    fixture.write(
        &format!("{JOB}/task__trial/artifacts/work/result.json"),
        &harbor_result(),
    )?;
    let catalog = Catalog::load(&fixture.0)?;
    ensure!(catalog.attempts.len() == 2);
    let attempt = catalog
        .attempts
        .iter()
        .find(|attempt| attempt.task == "org/task")
        .ok_or_else(|| eyre!("missing Harbor trial"))?;
    ensure!(attempt.status == "passed");
    ensure!(attempt.metrics.input_tokens == Some(100));
    ensure!(attempt.metrics.uncached_input_tokens == Some(60));
    ensure!(attempt.metrics.output_tokens == Some(20));
    ensure!(attempt.metrics.reasoning_tokens.is_none() && attempt.metrics.turns.is_none());
    ensure!(attempt.metrics.duration_ms == Some(2000) && attempt.metrics.cost_usd == Some(0.25));
    ensure!(
        catalog
            .attempts
            .iter()
            .any(|attempt| attempt.status == "interrupted")
    );
    ensure!(
        catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("incomplete"))
    );
    Ok(())
}

#[test]
fn harbor_controller_accounting_supplies_shared_token_and_context_definitions() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(
        &format!("{JOB}/kraai-run.json"),
        &json!({"identity": {
        "dataset": "benchmark@1", "model": "model", "spec": {"harness": "kraai"}}}),
    )?;
    let trial = format!("{JOB}/task__trial");
    fixture.write(&format!("{trial}/result.json"), &harbor_result())?;
    fixture.write(
        &format!("{trial}/kraai-controller/proxy-metrics.json"),
        &proxy(),
    )?;
    accounting(&fixture.0.join(&trial).join("kraai-controller"))?;
    let catalog = Catalog::load(&fixture.0)?;
    let attempt = catalog
        .attempts
        .first()
        .ok_or_else(|| eyre!("missing trial"))?;
    ensure!(attempt.metrics.input_tokens == Some(60));
    ensure!(attempt.metrics.output_tokens == Some(60));
    ensure!(attempt.metrics.final_context_tokens == Some(35));
    ensure!(attempt.metrics.cost_usd == Some(1.25));
    ensure!(
        attempt
            .logs
            .contains(&String::from("kraai-controller--proxy.events.jsonl"))
    );
    Ok(())
}

#[test]
fn stale_accounting_never_contributes_cost_or_complete_usage() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(
        &format!("{NATIVE}/result.json"),
        &native(json!({"proxy": proxy()})),
    )?;
    accounting(&fixture.0.join(NATIVE))?;
    fs::write(
        fixture.0.join(NATIVE).join("proxy.events.jsonl"),
        b"changed",
    )?;
    let catalog = Catalog::load(&fixture.0)?;
    let metrics = &catalog
        .attempts
        .first()
        .ok_or_else(|| eyre!("missing attempt"))?
        .metrics;
    ensure!(metrics.input_tokens.is_none() && metrics.cost_usd.is_none());
    ensure!(metrics.final_context_tokens.is_none());
    ensure!(
        catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("does not match"))
    );
    Ok(())
}

#[test]
fn known_log_names_are_bounded_and_workspace_files_are_not_exposed() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(&format!("{NATIVE}/result.json"), &native(json!({})))?;
    let directory = fixture.0.join(NATIVE);
    fs::write(
        directory.join("runner.stderr.log"),
        vec![b'x'; files::LOG_LIMIT as usize + 20],
    )?;
    fs::write(directory.join("secret.txt"), b"private")?;
    let catalog = Catalog::load(&fixture.0)?;
    let attempt = catalog
        .attempts
        .first()
        .ok_or_else(|| eyre!("missing attempt"))?;
    ensure!(!attempt.logs.iter().any(|name| name == "secret.txt"));
    ensure!(catalog.read_log(&attempt.id, "../secret.txt").is_err());
    ensure!(catalog.read_log(&attempt.id, "secret.txt").is_err());
    let log = catalog.read_log(&attempt.id, "runner.stderr.log")?;
    ensure!(log.truncated && log.content.len() == files::LOG_LIMIT as usize);
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinks_are_rejected_at_registration_and_at_read_time() -> Result<()> {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new()?;
    fixture.write(&format!("{NATIVE}/result.json"), &native(json!({})))?;
    let directory = fixture.0.join(NATIVE);
    fs::write(fixture.0.join("secret"), b"secret")?;
    symlink(
        fixture.0.join("secret"),
        directory.join("runner.stdout.log"),
    )?;
    fs::write(directory.join("runner.stderr.log"), b"allowed")?;
    let catalog = Catalog::load(&fixture.0)?;
    let attempt = catalog
        .attempts
        .first()
        .ok_or_else(|| eyre!("missing attempt"))?;
    ensure!(!attempt.logs.iter().any(|name| name == "runner.stdout.log"));
    fs::remove_file(directory.join("runner.stderr.log"))?;
    symlink(
        fixture.0.join("secret"),
        directory.join("runner.stderr.log"),
    )?;
    ensure!(catalog.read_log(&attempt.id, "runner.stderr.log").is_err());
    Ok(())
}

#[test]
fn model_settings_split_versions_and_task_changes_are_reported() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(&format!("{NATIVE}/result.json"), &native(json!({})))?;
    let mut revised = native(json!({}));
    set(&mut revised, "task_sha256", json!("updated-task"))?;
    set(&mut revised, "experiment_id", json!("revised"))?;
    fixture.write(
        "runs/task/codex/version/model/attempt-1/revised/result.json",
        &revised,
    )?;
    set(
        &mut revised,
        "provider_config_sha256",
        json!("different-settings"),
    )?;
    set(&mut revised, "experiment_id", json!("different-settings"))?;
    fixture.write(
        "runs/task/codex/version/model/attempt-2/settings/result.json",
        &revised,
    )?;
    set(
        &mut revised,
        "rust_environment_programs",
        json!(["cargo", "rustc"]),
    )?;
    set(
        &mut revised,
        "experiment_id",
        json!("different-environment"),
    )?;
    fixture.write(
        "runs/task/codex/version/model/attempt-3/environment/result.json",
        &revised,
    )?;
    let catalog = Catalog::load(&fixture.0)?;
    ensure!(catalog.versions.len() == 3);
    ensure!(
        catalog
            .versions
            .iter()
            .all(|version| version.label == "version")
    );
    ensure!(
        catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("different saved task"))
    );
    Ok(())
}

#[test]
fn canonical_native_result_replaces_archived_copy_regardless_of_status() -> Result<()> {
    for (canonical_status, archived_status) in [("passed", "runner_failed"), ("failed", "passed")] {
        let fixture = Fixture::new()?;
        let mut canonical = native(json!({}));
        set(&mut canonical, "status", json!(canonical_status))?;
        fixture.write(&format!("{NATIVE}/result.json"), &canonical)?;
        let mut archived = canonical.clone();
        set(&mut archived, "status", json!(archived_status))?;
        set(&mut archived, "completed_at_ms", json!(999))?;
        fixture.write("failures/host-bus-experiment/result.json", &archived)?;
        let catalog = Catalog::load(&fixture.0)?;
        ensure!(catalog.attempts.len() == 1 && catalog.versions.len() == 1);
        let attempt = catalog
            .attempts
            .first()
            .ok_or_else(|| eyre!("missing native result"))?;
        ensure!(attempt.status == canonical_status);
        let log = catalog.read_log(&attempt.id, "result.json")?;
        ensure!(serde_json::from_str::<Value>(&log.content)? == canonical);
    }
    Ok(())
}

#[test]
fn archived_experiments_without_committed_results_keep_only_latest_record() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut old = native(json!({}));
    set(&mut old, "status", json!("runner_failed"))?;
    fixture.write("failures/first-experiment/result.json", &old)?;
    let mut latest = old;
    set(&mut latest, "status", json!("controller_failed"))?;
    set(&mut latest, "completed_at_ms", json!(400))?;
    fixture.write("failures/latest-experiment/result.json", &latest)?;
    let catalog = Catalog::load(&fixture.0)?;
    ensure!(catalog.attempts.len() == 1);
    ensure!(
        catalog
            .attempts
            .first()
            .is_some_and(|attempt| attempt.status == "error")
    );
    Ok(())
}

#[test]
fn cancelled_harbor_trials_remain_interrupted_even_with_finish_time_and_reward() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(&format!("{JOB}/lock.json"), &json!({"trials": [{}]}))?;
    let mut result = harbor_result();
    set(
        &mut result,
        "exception_info",
        json!({"exception_type": "CancelledError", "exception_message": "cancelled"}),
    )?;
    fixture.write(&format!("{JOB}/task__trial/result.json"), &result)?;
    let catalog = Catalog::load(&fixture.0)?;
    ensure!(
        catalog
            .attempts
            .first()
            .is_some_and(|attempt| attempt.status == "interrupted")
    );
    Ok(())
}

#[test]
fn harbor_proxy_request_budgets_create_separate_versions() -> Result<()> {
    let fixture = Fixture::new()?;
    for (job, budget) in [("first", 10), ("second", 20)] {
        let directory = format!("public/benchmark/kraai/version/model/{job}");
        fixture.write(
            &format!("{directory}/kraai-run.json"),
            &json!({"identity": {
                "dataset": "benchmark@1", "model": "model", "spec": {"harness": "kraai",
                    "proxy_command": ["/bin/proxy", "--max-requests", budget.to_string()]}
            }}),
        )?;
        fixture.write(
            &format!("{directory}/task__trial/result.json"),
            &harbor_result(),
        )?;
    }
    let catalog = Catalog::load(&fixture.0)?;
    ensure!(catalog.versions.len() == 2 && catalog.attempts.len() == 2);
    Ok(())
}

#[test]
fn native_failure_reasons_use_recorded_process_outcomes() -> Result<()> {
    for (status, reason) in [
        ("runner_failed", "Runner timed out"),
        ("failed", "Grader 0 exited with code 7"),
    ] {
        let fixture = Fixture::new()?;
        let mut result = native(json!({}));
        set(&mut result, "status", json!(status))?;
        set(
            &mut result,
            "runner",
            json!({"command": ["runner"], "exit_code": null,
            "timed_out": true, "output_limit_exceeded": false, "duration_ms": 100}),
        )?;
        set(
            &mut result,
            "graders",
            json!([{"command": ["grader"], "exit_code": 7,
            "timed_out": false, "output_limit_exceeded": false, "duration_ms": 100}]),
        )?;
        fixture.write(&format!("{NATIVE}/result.json"), &result)?;
        let catalog = Catalog::load(&fixture.0)?;
        ensure!(
            catalog
                .attempts
                .first()
                .and_then(|attempt| attempt.error.as_deref())
                == Some(reason)
        );
    }
    Ok(())
}
