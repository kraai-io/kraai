use std::fs;

use color_eyre::eyre::{Result, ensure};
use serde_json::{Value, json};

use super::*;

struct TestJobs(PathBuf);

impl TestJobs {
    fn new() -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("kraai-eval-harbor-{}", ulid::Ulid::generate()));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn job(&self, name: &str, outcomes: &[(Option<f64>, Option<&str>, Value)]) -> Result<PathBuf> {
        let directory = self.0.join(name);
        fs::create_dir_all(&directory)?;
        let trial_lock = json!({
            "schema_version": 2,
            "task": {"name": "sample-task", "digest": format!("sha256:{}", "a".repeat(64)), "type": "git", "source": "terminal-bench@2.0", "git_url": "https://example.com/tasks", "git_commit_id": "a".repeat(40)},
            "agent": {"name": name, "model_name": "gpt-6-astra", "import_path": null, "load_trajectory": null, "override_timeout_sec": null, "kwargs": {"reasoning_effort": "low"}},
            "environment": {"type": "docker", "override_cpus": 2},
            "verifier": {"disable": false, "environment_mode": "shared"},
            "timeout_multiplier": 1.0,
        });
        write(
            &directory.join("lock.json"),
            &json!({
                "schema_version": 3, "harbor": {"version": "0.22.0"}, "n_concurrent_trials": 1,
                "retry": {"max_retries": 0}, "trials": vec![trial_lock.clone(); outcomes.len()],
            }),
        )?;
        write(
            &directory.join("result.json"),
            &json!({
                "id": format!("job-{name}"), "started_at": "2026-09-11T10:00:00Z", "finished_at": "2026-09-11T11:00:00Z",
                "n_total_trials": outcomes.len(), "stats": {"n_completed_trials": outcomes.len(), "n_retries": 0},
            }),
        )?;
        for (index, (reward, exception, context)) in outcomes.iter().enumerate() {
            let trial_name = format!("trial-{index}");
            let trial_dir = directory.join(&trial_name);
            fs::create_dir_all(&trial_dir)?;
            write(&trial_dir.join("lock.json"), &trial_lock)?;
            write(
                &trial_dir.join("result.json"),
                &json!({
                    "trial_name": trial_name, "task_name": "sample-task", "task_checksum": "b".repeat(64),
                    "config": {"job_id": format!("job-{name}"), "agent": trial_lock.field("/agent")},
                    "agent_info": {"name": name, "version": "1.2.3"}, "agent_result": context,
                    "verifier_result": reward.map(|reward| json!({"rewards": {"reward": reward}})),
                    "exception_info": exception.map(|exception| json!({"exception_type": exception})),
                    "started_at": format!("2026-09-11T10:00:{index:02}Z"), "finished_at": format!("2026-09-11T10:01:{index:02}Z"),
                    "agent_execution": {"started_at": format!("2026-09-11T10:00:{index:02}Z"), "finished_at": format!("2026-09-11T10:00:{:02}Z", index + 20)},
                }),
            )?;
        }
        Ok(directory)
    }
}

impl Drop for TestJobs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn mutate(path: &Path, edit: impl FnOnce(&mut Value) -> Result<()>) -> Result<()> {
    let mut value: Value = serde_json::from_slice(&fs::read(path)?)?;
    edit(&mut value)?;
    write(path, &value)
}

fn replace(value: &mut Value, pointer: &str, replacement: Value) -> Result<()> {
    let field = value
        .pointer_mut(pointer)
        .ok_or_else(|| color_eyre::eyre::eyre!("missing fixture field {pointer}"))?;
    *field = replacement;
    Ok(())
}

fn usage(input: Option<u64>, cache: Option<u64>, output: Option<u64>) -> Value {
    json!({"n_input_tokens": input, "n_cache_tokens": cache, "n_output_tokens": output})
}

fn rejects(result: Result<HarborComparisonResult>, message: &str) -> Result<()> {
    let error = result
        .err()
        .ok_or_else(|| color_eyre::eyre::eyre!("expected rejection: {message}"))?;
    ensure!(
        format!("{error:#}").contains(message),
        "unexpected rejection: {error:#}"
    );
    Ok(())
}

#[test]
fn harbor_pairs_preserve_errors_and_separate_efficiency_on_completed_solutions() -> Result<()> {
    let jobs = TestJobs::new()?;
    let left = jobs.job(
        "left",
        &[
            (Some(1.0), None, usage(Some(100), Some(20), Some(20))),
            (
                None,
                Some("AgentTimeoutError"),
                usage(Some(1), Some(0), Some(0)),
            ),
            (
                None,
                Some("EnvironmentStartTimeoutError"),
                usage(Some(50), Some(20), Some(5)),
            ),
        ],
    )?;
    let right = jobs.job(
        "right",
        &[
            (Some(1.0), None, usage(Some(70), Some(60), Some(30))),
            (Some(1.0), None, usage(Some(200), Some(100), Some(20))),
            (Some(1.0), None, usage(Some(300), Some(150), Some(25))),
        ],
    )?;
    let result = compare_harbor_jobs(&left, &right)?;
    ensure!(result.paired_runs == 3 && result.evaluated_pairs == 2 && result.invalid_pairs == 1);
    ensure!(result.left_passed == 1 && result.right_passed == 3 && result.right_wins == 1);
    ensure!(result.both_passed == 1);
    ensure!(result.all_attempts.total_tokens.left_total == 176);
    ensure!(result.all_attempts.uncached_input_tokens.left_total == 111);
    ensure!(result.both_passed_efficiency.total_tokens.left_total == 120);
    ensure!(result.both_passed_efficiency.total_tokens.right_total == 100);
    ensure!(
        result
            .both_passed_efficiency
            .uncached_input_tokens
            .right_total
            == 10
    );
    ensure!(result.all_attempts.reasoning_tokens.samples == 0);
    ensure!(result.all_attempts.proxy_requests.samples == 0);
    ensure!(
        result
            .runs
            .get(2)
            .ok_or_else(|| color_eyre::eyre::eyre!("missing compared run"))?
            .outcome
            == PairOutcome::Invalid
    );
    ensure!(
        result
            .runs
            .get(2)
            .ok_or_else(|| color_eyre::eyre::eyre!("missing compared run"))?
            .left
            .exception_type
            .as_deref()
            == Some("EnvironmentStartTimeoutError")
    );
    ensure!(!result.wall_clock_reliable);
    let display = format_harbor_comparison(&result);
    ensure!(
        display.contains("Planned attempts: 3") && display.contains("Both harnesses passed: 1")
    );
    ensure!(
        display.contains("Reasoning tokens: n/a") && display.contains("Timing is observational")
    );
    Ok(())
}

#[test]
fn harbor_preserves_partial_usage_without_inventing_cached_or_reasoning_tokens() -> Result<()> {
    let jobs = TestJobs::new()?;
    let outcomes = [
        (Some(1.0), None, usage(Some(0), Some(0), Some(0))),
        (Some(1.0), None, usage(Some(30), None, Some(10))),
        (Some(1.0), None, usage(None, None, None)),
    ];
    let left = jobs.job("left", &outcomes)?;
    let right = jobs.job("right", &outcomes)?;
    let result = compare_harbor_jobs(&left, &right)?;
    ensure!(result.both_passed == 3);
    ensure!(
        result.all_attempts.total_tokens.samples == 2
            && result.all_attempts.total_tokens.left_total == 40
    );
    ensure!(
        result.all_attempts.cache_read_tokens.samples == 1
            && result.all_attempts.cache_read_tokens.left_mean == Some(0.0)
    );
    ensure!(result.all_attempts.uncached_input_tokens.samples == 1);
    ensure!(result.all_attempts.reasoning_tokens.left_mean.is_none());
    ensure!(
        result
            .runs
            .get(1)
            .ok_or_else(|| color_eyre::eyre::eyre!("missing compared run"))?
            .left
            .metrics
            .uncached_input_tokens
            .is_none()
    );
    ensure!(
        result
            .runs
            .get(2)
            .ok_or_else(|| color_eyre::eyre::eyre!("missing compared run"))?
            .left
            .metrics
            .total_tokens
            .is_none()
    );
    Ok(())
}

#[test]
fn harbor_rejects_missing_attempts_unfinished_jobs_and_lost_retry_usage() -> Result<()> {
    let jobs = TestJobs::new()?;
    let outcomes = [(Some(1.0), None, usage(Some(1), Some(0), Some(0)))];
    let left = jobs.job("left", &outcomes)?;
    let right = jobs.job("right", &outcomes)?;
    fs::remove_file(right.join("trial-0/result.json"))?;
    rejects(
        compare_harbor_jobs(&left, &right),
        "missing planned trial results",
    )?;
    let right = jobs.job("right", &outcomes)?;
    mutate(&right.join("result.json"), |result| {
        replace(result, "/finished_at", Value::Null)
    })?;
    rejects(compare_harbor_jobs(&left, &right), "unfinished")?;
    let right = jobs.job("right", &outcomes)?;
    mutate(&right.join("result.json"), |result| {
        replace(result, "/stats/n_retries", json!(1))
    })?;
    rejects(compare_harbor_jobs(&left, &right), "retry usage")?;
    Ok(())
}

#[test]
fn harbor_rejects_changed_pins_models_and_constraints() -> Result<()> {
    let jobs = TestJobs::new()?;
    let outcomes = [(Some(1.0), None, usage(Some(1), Some(0), Some(0)))];
    let left = jobs.job("left", &outcomes)?;
    let changes = [
        ("pinned SHA-256", "/task/digest", json!("latest")),
        ("pinned Git commit", "/task/git_commit_id", Value::Null),
        (
            "task populations differ",
            "/task/digest",
            json!(format!("sha256:{}", "c".repeat(64))),
        ),
        (
            "execution settings differ",
            "/environment/override_cpus",
            json!(4),
        ),
        (
            "execution settings differ",
            "/agent/override_timeout_sec",
            json!(30),
        ),
        (
            "actual model settings",
            "/agent/kwargs/reasoning_effort",
            json!("high"),
        ),
        (
            "actual model settings",
            "/agent/kwargs/reasoning_effort",
            Value::Null,
        ),
        ("verifier is disabled", "/verifier/disable", json!(true)),
        (
            "loaded trajectories",
            "/agent/load_trajectory",
            json!("/tmp/reference-session.jsonl"),
        ),
    ];
    for (message, pointer, value) in changes {
        let right = jobs.job("right", &outcomes)?;
        mutate(&right.join("lock.json"), |lock| {
            replace(lock, &format!("/trials/0{pointer}"), value.clone())
        })?;
        mutate(&right.join("trial-0/lock.json"), |lock| {
            replace(lock, pointer, value)
        })?;
        rejects(compare_harbor_jobs(&left, &right), message)?;
    }
    let right = jobs.job("right", &outcomes)?;
    mutate(&right.join("lock.json"), |lock| {
        replace(lock, "/harbor/version", json!("latest"))
    })?;
    rejects(compare_harbor_jobs(&left, &right), "pinned version")?;
    Ok(())
}

fn add_proxy(job: &Path, tokens: u64) -> Result<()> {
    let directory = job.join("trial-0/kraai-controller");
    fs::create_dir_all(&directory)?;
    write(
        &directory.join("proxy-metrics.json"),
        &json!({
            "requests": 4, "successful_requests": 4, "failed_requests": 0, "duration_ms": 10,
            "usage": {"total_tokens": tokens + 14, "input_tokens": tokens, "cache_read_tokens": 10, "output_tokens": 3, "reasoning_tokens": 1},
        }),
    )?;
    write(
        &directory.join("proxy-identity.json"),
        &json!({
            "transport_revision": 1, "max_requests": 64, "kind": "codex_subscription", "upstream": "https://example.com", "allowed_paths": ["/responses"],
        }),
    )?;
    fs::write(
        directory.join("proxy.events.jsonl"),
        format!(
            "{}\n",
            json!({
                "model": "gpt-6-astra", "reasoning_effort": "low", "service_tier": "priority"
            })
        ),
    )?;
    Ok(())
}

#[test]
fn harbor_controller_proxy_usage_is_authoritative_and_configuration_must_match() -> Result<()> {
    let jobs = TestJobs::new()?;
    let outcomes = [(Some(1.0), None, usage(Some(9_999), Some(0), Some(999)))];
    let left = jobs.job("left", &outcomes)?;
    let right = jobs.job("right", &outcomes)?;
    add_proxy(&left, 20)?;
    add_proxy(&right, 50)?;
    let comparison = compare_harbor_jobs(&left, &right)?;
    ensure!(comparison.all_attempts.total_tokens.left_total == 34);
    ensure!(comparison.all_attempts.input_tokens.left_total == 30);
    ensure!(comparison.all_attempts.uncached_input_tokens.left_total == 20);
    ensure!(comparison.all_attempts.output_tokens.left_total == 4);
    ensure!(comparison.all_attempts.reasoning_tokens.left_total == 1);
    ensure!(comparison.all_attempts.proxy_requests.left_total == 4);
    ensure!(
        comparison
            .runs
            .first()
            .ok_or_else(|| color_eyre::eyre::eyre!("missing compared run"))?
            .left
            .metrics
            .usage_source
            == "controller_proxy"
    );
    mutate(
        &right.join("trial-0/kraai-controller/proxy-identity.json"),
        |identity| replace(identity, "/max_requests", json!(32)),
    )?;
    rejects(
        compare_harbor_jobs(&left, &right),
        "proxy configuration differs",
    )?;
    add_proxy(&right, 50)?;
    fs::write(
        right.join("trial-0/kraai-controller/proxy.events.jsonl"),
        "{\"model\":\"other-model\",\"reasoning_effort\":\"low\",\"service_tier\":\"priority\"}\n",
    )?;
    rejects(compare_harbor_jobs(&left, &right), "actual model settings")?;
    Ok(())
}

#[test]
fn harbor_verifier_reward_preserves_success_after_agent_timeout_or_nonzero_exit() -> Result<()> {
    let jobs = TestJobs::new()?;
    let left = jobs.job(
        "left",
        &[
            (
                Some(1.0),
                Some("AgentTimeoutError"),
                usage(Some(10), Some(0), Some(1)),
            ),
            (
                Some(1.0),
                Some("NonZeroAgentExitCodeError"),
                usage(Some(10), Some(0), Some(1)),
            ),
        ],
    )?;
    let right = jobs.job(
        "right",
        &[
            (Some(1.0), None, usage(Some(10), Some(0), Some(1))),
            (Some(1.0), None, usage(Some(10), Some(0), Some(1))),
        ],
    )?;
    let result = compare_harbor_jobs(&left, &right)?;
    ensure!(result.left_passed == 2 && result.right_passed == 2 && result.both_passed == 2);
    ensure!(result.left_wins == 0 && result.right_wins == 0 && result.ties == 2);
    ensure!(
        result
            .runs
            .iter()
            .all(|run| run.left.exception_type.is_some())
    );
    Ok(())
}

#[test]
fn harbor_adapter_requires_private_controller_records_and_ignores_shared_usage() -> Result<()> {
    let jobs = TestJobs::new()?;
    let outcomes = [(Some(1.0), None, usage(Some(9_999), Some(0), Some(999)))];
    let left = jobs.job("left", &outcomes)?;
    let right = jobs.job("right", &outcomes)?;
    for (job, harness) in [(&left, "left"), (&right, "right")] {
        use_profile_adapter(job)?;
        add_proxy(job, 2_000)?;
        fs::rename(
            job.join("trial-0/kraai-controller"),
            job.join("trial-0/agent"),
        )?;
        write(
            &job.join("trial-0/agent/runner-metrics.json"),
            &json!({
                "schema_version": 1, "harness": harness, "model": "gpt-6-astra", "bundle_sha256": "1.2.3"
            }),
        )?;
    }
    rejects(
        compare_harbor_jobs(&left, &right),
        "private controller runner metadata",
    )?;
    for job in [&left, &right] {
        fs::create_dir_all(job.join("trial-0/kraai-controller"))?;
        fs::copy(
            job.join("trial-0/agent/runner-metrics.json"),
            job.join("trial-0/kraai-controller/runner-metrics.json"),
        )?;
    }
    rejects(
        compare_harbor_jobs(&left, &right),
        "private controller proxy metrics",
    )?;
    for job in [&left, &right] {
        add_proxy(job, 20)?;
    }
    let result = compare_harbor_jobs(&left, &right)?;
    ensure!(result.all_attempts.total_tokens.left_total == 34);
    for job in [&left, &right] {
        mutate(
            &job.join("trial-0/kraai-controller/proxy-metrics.json"),
            |proxy| {
                replace(
                    proxy,
                    "/usage",
                    serde_json::to_value(crate::UsageMetrics::default())?,
                )
            },
        )?;
    }
    let result = compare_harbor_jobs(&left, &right)?;
    ensure!(result.all_attempts.total_tokens.samples == 0);
    ensure!(result.all_attempts.proxy_requests.left_total == 4);
    ensure!(
        result
            .runs
            .iter()
            .all(|run| run.left.metrics.usage_source == "controller_proxy_usage_unavailable")
    );
    Ok(())
}

fn use_profile_adapter(job: &Path) -> Result<()> {
    for (file, pointer) in [
        ("lock.json", "/trials/0/agent/import_path"),
        ("trial-0/lock.json", "/agent/import_path"),
    ] {
        mutate(&job.join(file), |lock| {
            replace(lock, pointer, json!("kraai_harbor.agent:ProfileAgent"))
        })?;
    }
    Ok(())
}

#[test]
fn harbor_adapter_keeps_infrastructure_failures_without_controller_files() -> Result<()> {
    let jobs = TestJobs::new()?;
    let left = jobs.job(
        "left",
        &[(
            None,
            Some("EnvironmentStartTimeoutError"),
            usage(Some(99_999), Some(999), Some(99)),
        )],
    )?;
    let right = jobs.job(
        "right",
        &[(Some(1.0), None, usage(Some(10), Some(0), Some(1)))],
    )?;
    use_profile_adapter(&left)?;
    let assert_infrastructure = || -> Result<()> {
        let result = compare_harbor_jobs(&left, &right)?;
        ensure!(
            result.paired_runs == 1 && result.invalid_pairs == 1 && result.evaluated_pairs == 0
        );
        ensure!(result.left_wins == 0 && result.right_wins == 0 && result.both_passed == 0);
        ensure!(result.left_passed == 0 && result.right_passed == 1);
        ensure!(result.all_attempts.total_tokens.samples == 0);
        let run = result
            .runs
            .first()
            .ok_or_else(|| color_eyre::eyre::eyre!("missing infrastructure pair"))?;
        ensure!(run.left.status == HarborTrialStatus::InfrastructureError);
        ensure!(
            run.left.metrics.total_tokens.is_none() && run.left.metrics.proxy_requests.is_none()
        );
        ensure!(run.left.metrics.usage_source == "controller_proxy_usage_unavailable");
        Ok(())
    };
    assert_infrastructure()?;
    fs::create_dir_all(left.join("trial-0/kraai-controller"))?;
    write(
        &left.join("trial-0/kraai-controller/runner-metrics.json"),
        &json!({
            "schema_version": 1, "harness": "left", "model": "gpt-6-astra", "bundle_sha256": "1.2.3"
        }),
    )?;
    mutate(&left.join("trial-0/result.json"), |result| {
        replace(
            result,
            "/exception_info/exception_type",
            json!("AgentAuthenticationError"),
        )
    })?;
    assert_infrastructure()?;
    Ok(())
}
