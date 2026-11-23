mod file_access;
mod scanning;
mod trajectory;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Catalog, files};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kraai-viewer-{}", ulid::Ulid::generate()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn write(&self, path: &str, value: &Value) -> PathBuf {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().expect("missing parent")).unwrap();
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const FOLDER: &str = "in-house/model/2026-01-01-codex-0123abcd";
const NATIVE: &str = "in-house/model/2026-01-01-codex-0123abcd/task/attempt-1";
const JOB: &str = "benchmark/model/2026-01-01-codex-0123abcd/harbor";

fn set(value: &mut Value, key: &str, field: Value) {
    value
        .as_object_mut()
        .expect("expected fixture object")
        .insert(key.to_owned(), field);
}

fn native(metrics: Value) -> Value {
    json!({
        "schema_version": 6, "experiment_id": "experiment", "artifact_path": NATIVE,
        "task_id": "task", "harness_name": "codex", "model_label": "model", "attempt": 1,
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

fn accounting(directory: &Path) {
    let events = b"{\"method\":\"POST\",\"path\":\"/responses\"}\n";
    fs::create_dir_all(directory).unwrap();
    fs::write(directory.join("proxy.events.jsonl"), events).unwrap();
    fs::write(
        directory.join("request-accounting.json"),
        serde_json::to_vec(&json!({
            "events_sha256": crate::cache::hash_chunks(&[events]),
            "model_requests": 2, "unrecorded_requests": 0,
            "context": {"samples": 2, "total_input_tokens": 60, "min_input_tokens": 25,
                "peak_input_tokens": 35, "last_input_tokens": 35, "mean_input_tokens": 30.0},
            "requests": [request(0), request(1)]
        }))
        .unwrap(),
    )
    .unwrap();
}

fn request(index: usize) -> Value {
    json!({"event_index": index, "model": "model",
        "usage": {"total_tokens": 625, "input_tokens": 625, "output_tokens": 0,
            "reasoning_tokens": 0, "cache_read_tokens": 0}})
}

fn prices(fixture: &Fixture) {
    fixture.write(
        "prices.json",
        &json!({"schema_version": 1, "models": {
            "model": {"input": 1000.0, "output": 2000.0, "cache_read": 500.0}}}),
    );
}

fn suite() -> Value {
    json!({"schema_version": 2, "artifact_path": FOLDER,
        "harness_name": "codex", "runner_version": "version", "model_label": "model",
        "started_at_ms": 100, "completed_at_ms": 300, "duration_ms": 200,
        "requested_runs": 1, "evaluated_runs": 1, "passed_runs": 1, "failed_runs": 0,
        "controller_failures": 0, "launch_failures": 0, "success_rate": 1.0,
        "wall_time_ms": {"samples": 0},
        "total_tokens": {"total": 0, "distribution": {"samples": 0}},
        "used_context_tokens": {"total": 0, "distribution": {"samples": 0}},
        "runs": [{"task_id": "task", "attempt": 1, "status": "passed",
            "experiment_id": "experiment", "artifact_path": NATIVE, "duration_ms": 200},
            {"task_id": "other", "attempt": 1, "error": "launch failed"}]
    })
}

fn harbor_result() -> Value {
    json!({"id": "trial", "task_name": "org/task", "trial_name": "task__trial",
        "agent_info": {"name": "codex", "version": "0.1"},
        "config": {"agent": {"model_name": "provider/model"}},
        "agent_result": {"n_input_tokens": 100, "n_cache_tokens": 40,
            "n_output_tokens": 20, "cost_usd": 0.25},
        "verifier_result": {"rewards": {"reward": 1.0}},
        "started_at": "2026-09-20T10:00:00Z", "finished_at": "2026-09-20T10:00:02Z"})
}

#[test]
fn discovers_saved_attempts_and_summary_launch_failures_once() {
    let fixture = Fixture::new();
    fixture.write(
        &format!("{NATIVE}/result.json"),
        &native(json!({"proxy": proxy(),
        "harness": {"schema_version": 1, "turns": 2, "final_context_tokens": 99}})),
    );
    accounting(&fixture.0.join(NATIVE));
    fixture.write(&format!("{FOLDER}/summary.json"), &suite());
    prices(&fixture);
    let catalog = Catalog::load(&fixture.0).unwrap();
    assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
    assert!(catalog.versions.len() == 1 && catalog.attempts.len() == 2);
    assert!(
        catalog
            .attempts
            .iter()
            .any(|attempt| attempt.task == "other" && attempt.status == "error")
    );
    let attempt = catalog
        .attempts
        .iter()
        .find(|attempt| attempt.task == "task")
        .expect("missing attempt");
    let metrics = &attempt.metrics;
    assert!(metrics.input_tokens == Some(60));
    assert!(metrics.cached_input_tokens == Some(30) && metrics.uncached_input_tokens == Some(20));
    assert!(metrics.output_tokens == Some(60) && metrics.reasoning_tokens == Some(20));
    assert!(metrics.final_context_tokens == Some(35) && metrics.turns == Some(2));
    assert!(metrics.requests == Some(2) && metrics.duration_ms == Some(200));
    assert!(metrics.cost_usd == Some(1.25));
    assert!(
        catalog
            .versions
            .first()
            .is_some_and(|version| version.harness == "codex"
                && version.label == "0123abcd"
                && version.model.as_deref() == Some("model")
                && version.benchmark == "in-house"
                && version.latest_at_ms == Some(100))
    );
}

#[test]
fn unavailable_measurements_remain_null_and_corrupt_results_remain_visible() {
    let fixture = Fixture::new();
    fixture.write(&format!("{NATIVE}/result.json"), &native(json!({})));
    let broken = fixture.write(
        &format!("{FOLDER}/broken/attempt-1/result.json"),
        &Value::Null,
    );
    fs::write(broken, b"{partial").unwrap();
    fs::create_dir_all(fixture.0.join(FOLDER).join("missing/attempt-2")).unwrap();
    fixture.write(
        ".cache/staging/in-house/model/2026-01-01-codex-0123abcd/task/attempt-3/result.json",
        &native(json!({})),
    );
    let catalog = Catalog::load(&fixture.0).unwrap();
    assert!(catalog.attempts.len() == 3);
    let saved = catalog
        .attempts
        .iter()
        .find(|attempt| attempt.task == "task")
        .expect("missing saved attempt");
    let metrics = serde_json::to_value(&saved.metrics).unwrap();
    assert!(
        metrics.get("input_tokens").is_some_and(Value::is_null)
            && metrics.get("cost_usd").is_some_and(Value::is_null)
    );
    assert!(
        catalog
            .attempts
            .iter()
            .filter(|attempt| attempt.status == "interrupted")
            .count()
            == 2
    );
    assert!(catalog.warnings.len() == 2);
}

#[test]
fn harbor_preserves_unknown_metrics_and_retains_unfinished_trials() {
    let fixture = Fixture::new();
    fixture.write(
        &format!("{JOB}/lock.json"),
        &json!({"trials": [{}, {}, {}]}),
    );
    fixture.write(&format!("{JOB}/task__trial/result.json"), &harbor_result());
    fixture.write(
        &format!("{JOB}/pending__trial/lock.json"),
        &json!({"task": {"name": "org/pending"}}),
    );
    fixture.write(
        &format!("{JOB}/task__trial/artifacts/work/result.json"),
        &harbor_result(),
    );
    prices(&fixture);
    let catalog = Catalog::load(&fixture.0).unwrap();
    assert!(catalog.attempts.len() == 2);
    let attempt = catalog
        .attempts
        .iter()
        .find(|attempt| attempt.task == "org/task")
        .expect("missing Harbor trial");
    assert!(attempt.status == "passed");
    assert!(attempt.metrics.input_tokens == Some(100));
    assert!(attempt.metrics.uncached_input_tokens == Some(60));
    assert!(attempt.metrics.output_tokens == Some(20));
    assert!(attempt.metrics.reasoning_tokens.is_none() && attempt.metrics.turns.is_none());
    assert!(attempt.metrics.duration_ms == Some(2000) && attempt.metrics.cost_usd == Some(0.12));
    assert!(
        catalog
            .attempts
            .iter()
            .any(|attempt| attempt.status == "interrupted")
    );
    assert!(
        catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("incomplete"))
    );
}

#[test]
fn harbor_controller_accounting_supplies_shared_token_and_context_definitions() {
    let fixture = Fixture::new();
    fixture.write(
        &format!("{JOB}/kraai-run.json"),
        &json!({"identity": {
        "dataset": "benchmark@1", "model": "model", "spec": {"harness": "kraai"}}}),
    );
    let trial = format!("{JOB}/task__trial");
    fixture.write(&format!("{trial}/result.json"), &harbor_result());
    fixture.write(
        &format!("{trial}/kraai-controller/proxy-metrics.json"),
        &proxy(),
    );
    accounting(&fixture.0.join(&trial).join("kraai-controller"));
    prices(&fixture);
    let catalog = Catalog::load(&fixture.0).unwrap();
    let attempt = catalog.attempts.first().expect("missing trial");
    assert!(attempt.metrics.input_tokens == Some(60));
    assert!(attempt.metrics.output_tokens == Some(60));
    assert!(attempt.metrics.final_context_tokens == Some(35));
    assert!(attempt.metrics.cost_usd == Some(1.25));
    assert!(
        attempt
            .logs
            .contains(&String::from("kraai-controller--proxy.events.jsonl"))
    );
}

#[test]
fn costs_need_a_saved_price_and_ignore_costs_harbor_or_the_harness_report() {
    let fixture = Fixture::new();
    fixture.write(
        &format!("{NATIVE}/result.json"),
        &native(json!({"proxy": proxy()})),
    );
    accounting(&fixture.0.join(NATIVE));
    fixture.write(&format!("{JOB}/task__trial/result.json"), &harbor_result());
    let catalog = Catalog::load(&fixture.0).unwrap();
    assert!(catalog.attempts.len() == 2);
    assert!(catalog.attempts.iter().all(
        |attempt| attempt.metrics.input_tokens.is_some() && attempt.metrics.cost_usd.is_none()
    ));
    fixture.write(
        "prices.json",
        &json!({"schema_version": 1, "models": {"other": {"input": 1.0, "output": 1.0}}}),
    );
    let catalog = Catalog::load(&fixture.0).unwrap();
    assert!(
        catalog
            .attempts
            .iter()
            .all(|attempt| attempt.metrics.cost_usd.is_none())
    );
}

#[test]
fn stale_accounting_never_contributes_cost_or_complete_usage() {
    let fixture = Fixture::new();
    fixture.write(
        &format!("{NATIVE}/result.json"),
        &native(json!({"proxy": proxy()})),
    );
    accounting(&fixture.0.join(NATIVE));
    fs::write(
        fixture.0.join(NATIVE).join("proxy.events.jsonl"),
        b"changed",
    )
    .unwrap();
    prices(&fixture);
    let catalog = Catalog::load(&fixture.0).unwrap();
    let metrics = &catalog.attempts.first().expect("missing attempt").metrics;
    assert!(metrics.input_tokens.is_none() && metrics.cost_usd.is_none());
    assert!(metrics.final_context_tokens.is_none());
    assert!(
        catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("does not match"))
    );
}

#[test]
fn known_log_names_are_bounded_and_workspace_files_are_not_exposed() {
    let fixture = Fixture::new();
    fixture.write(&format!("{NATIVE}/result.json"), &native(json!({})));
    let directory = fixture.0.join(NATIVE);
    fs::write(
        directory.join("runner.stderr.log"),
        vec![b'x'; files::LOG_LIMIT as usize + 20],
    )
    .unwrap();
    fs::write(directory.join("secret.txt"), b"private").unwrap();
    let catalog = Catalog::load(&fixture.0).unwrap();
    let attempt = catalog.attempts.first().expect("missing attempt");
    assert!(!attempt.logs.iter().any(|name| name == "secret.txt"));
    assert!(catalog.read_log(&attempt.id, "../secret.txt").is_err());
    assert!(catalog.read_log(&attempt.id, "secret.txt").is_err());
    let log = catalog.read_log(&attempt.id, "runner.stderr.log").unwrap();
    assert!(log.truncated && log.content.len() == files::LOG_LIMIT as usize);
}

#[cfg(unix)]
#[test]
fn symlinks_are_rejected_at_registration_and_at_read_time() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fixture.write(&format!("{NATIVE}/result.json"), &native(json!({})));
    let directory = fixture.0.join(NATIVE);
    fs::write(fixture.0.join("secret"), b"secret").unwrap();
    symlink(
        fixture.0.join("secret"),
        directory.join("runner.stdout.log"),
    )
    .unwrap();
    fs::write(directory.join("runner.stderr.log"), b"allowed").unwrap();
    let catalog = Catalog::load(&fixture.0).unwrap();
    let attempt = catalog.attempts.first().expect("missing attempt");
    assert!(!attempt.logs.iter().any(|name| name == "runner.stdout.log"));
    fs::remove_file(directory.join("runner.stderr.log")).unwrap();
    symlink(
        fixture.0.join("secret"),
        directory.join("runner.stderr.log"),
    )
    .unwrap();
    assert!(catalog.read_log(&attempt.id, "runner.stderr.log").is_err());
}

#[test]
fn one_folder_is_one_version_and_task_changes_are_reported() {
    let fixture = Fixture::new();
    fixture.write(&format!("{NATIVE}/result.json"), &native(json!({})));
    let mut revised = native(json!({}));
    set(&mut revised, "task_sha256", json!("updated-task"));
    set(&mut revised, "provider_config_sha256", json!("different"));
    fixture.write(&format!("{FOLDER}/task/attempt-2/result.json"), &revised);
    fixture.write(
        "in-house/model/2026-02-01-codex-89abcdef/task/attempt-1/result.json",
        &native(json!({})),
    );
    let catalog = Catalog::load(&fixture.0).unwrap();
    assert!(catalog.versions.len() == 2 && catalog.attempts.len() == 3);
    assert!(
        catalog
            .versions
            .iter()
            .any(|version| version.harness == "codex" && version.label == "89abcdef")
    );
    assert!(
        catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("different saved task"))
    );
}

#[test]
fn cancelled_harbor_trials_remain_interrupted_even_with_finish_time_and_reward() {
    let fixture = Fixture::new();
    fixture.write(&format!("{JOB}/lock.json"), &json!({"trials": [{}]}));
    let mut result = harbor_result();
    set(
        &mut result,
        "exception_info",
        json!({"exception_type": "CancelledError", "exception_message": "cancelled"}),
    );
    fixture.write(&format!("{JOB}/task__trial/result.json"), &result);
    let catalog = Catalog::load(&fixture.0).unwrap();
    assert!(
        catalog
            .attempts
            .first()
            .is_some_and(|attempt| attempt.status == "interrupted")
    );
}

#[test]
fn harbor_attempts_count_from_one_per_task() {
    let fixture = Fixture::new();
    for (trial, started) in [
        ("task__a", "2026-09-20T10:00:00Z"),
        ("task__b", "2026-09-20T11:00:00Z"),
    ] {
        let mut result = harbor_result();
        set(&mut result, "started_at", json!(started));
        fixture.write(&format!("{JOB}/{trial}/result.json"), &result);
    }
    fixture.write(
        "benchmark/model/2026-01-01-codex-0123abcd/interrupted/task__c/result.json",
        &harbor_result(),
    );
    let catalog = Catalog::load(&fixture.0).unwrap();
    let mut attempts = catalog
        .attempts
        .iter()
        .map(|attempt| attempt.attempt)
        .collect::<Vec<_>>();
    attempts.sort_unstable();
    assert!(attempts == [1, 2], "{attempts:?}");
    assert!(catalog.versions.len() == 1);
}

#[test]
fn native_failure_reasons_use_recorded_process_outcomes() {
    for (status, reason) in [
        ("runner_failed", "Runner timed out"),
        ("failed", "Grader 0 exited with code 7"),
    ] {
        let fixture = Fixture::new();
        let mut result = native(json!({}));
        set(&mut result, "status", json!(status));
        set(
            &mut result,
            "runner",
            json!({"command": ["runner"], "exit_code": null,
            "timed_out": true, "output_limit_exceeded": false, "duration_ms": 100}),
        );
        set(
            &mut result,
            "graders",
            json!([{"command": ["grader"], "exit_code": 7,
            "timed_out": false, "output_limit_exceeded": false, "duration_ms": 100}]),
        );
        fixture.write(&format!("{NATIVE}/result.json"), &result);
        let catalog = Catalog::load(&fixture.0).unwrap();
        assert!(
            catalog
                .attempts
                .first()
                .and_then(|attempt| attempt.error.as_deref())
                == Some(reason)
        );
    }
}
