use super::*;
use crate::suite::{Distribution, TokenSummary};
use crate::{
    EvaluationMetrics, HarnessMetrics, NetworkPolicy, ProcessRecord, ProxyRecord, SandboxRecord,
    UsageMetrics,
};

struct TestCache(PathBuf);

impl TestCache {
    fn new() -> Result<Self> {
        let root =
            std::env::temp_dir().join(format!("kraai-eval-compare-{}", ulid::Ulid::generate()));
        fs::create_dir(&root)?;
        Ok(Self(root))
    }

    fn write_suite(
        &self,
        harness: &str,
        results: &[RunResult],
        launches: &[u64],
    ) -> Result<PathBuf> {
        let mut runs = Vec::new();
        for result in results {
            let directory = self.0.join(&result.artifact_path);
            fs::create_dir_all(&directory)?;
            fs::write(directory.join("result.json"), serde_json::to_vec(result)?)?;
            runs.push(SuiteRunResult {
                task_id: Some(result.task_id.clone()),
                attempt: result.attempt,
                status: Some(result.status.clone()),
                experiment_id: Some(result.experiment_id.clone()),
                artifact_path: Some(result.artifact_path.clone()),
                duration_ms: Some(result.duration_ms),
                usage: result.metrics.usage().cloned(),
                error: None,
            });
        }
        runs.extend(launches.iter().map(|attempt| SuiteRunResult {
            task_id: Some(String::from("sample-task")),
            attempt: *attempt,
            status: None,
            experiment_id: None,
            artifact_path: None,
            duration_ms: None,
            usage: None,
            error: Some(String::from("runner is unavailable")),
        }));
        let artifact_path = PathBuf::from("suites").join(harness).join("suite-id");
        let suite = SuiteResult {
            schema_version: 1,
            suite_id: format!("suite-{harness}"),
            artifact_path: artifact_path.clone(),
            harness_name: harness.to_owned(),
            runner_version: format!("version-{harness}"),
            model_label: Some(String::from("test-model")),
            started_at_ms: 0,
            completed_at_ms: 0,
            duration_ms: 0,
            requested_runs: runs.len() as u64,
            evaluated_runs: 0,
            passed_runs: 0,
            failed_runs: 0,
            controller_failures: 0,
            launch_failures: 0,
            success_rate: None,
            wall_time_ms: Distribution::default(),
            total_tokens: TokenSummary::default(),
            used_context_tokens: TokenSummary::default(),
            runs,
        };
        let directory = self.0.join(artifact_path);
        fs::create_dir_all(&directory)?;
        let path = directory.join("summary.json");
        fs::write(&path, serde_json::to_vec(&suite)?)?;
        Ok(path)
    }
}

impl Drop for TestCache {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(harness: &str, attempt: u64, status: RunStatus) -> RunResult {
    RunResult {
        schema_version: 6,
        experiment_id: format!("experiment-{harness}-{attempt}"),
        artifact_path: PathBuf::from("runs")
            .join(harness)
            .join(attempt.to_string()),
        task_id: String::from("sample-task"),
        harness_name: harness.to_owned(),
        model_label: Some(String::from("test-model")),
        attempt,
        runner_version: format!("version-{harness}"),
        runner_artifact_sha256: format!("artifact-{harness}"),
        task_sha256: String::from("task-hash"),
        grader_sha256: String::from("grader-hash"),
        sandbox: SandboxRecord {
            backend: String::from("bubblewrap+systemd-cgroup-v2"),
            network: NetworkPolicy::Enabled,
            environment_cleared: true,
            max_memory_bytes: 1024,
            max_processes: 16,
            cpu_quota_percent: 100,
        },
        status,
        runner: Some(ProcessRecord {
            command: vec![harness.to_owned()],
            exit_code: Some(0),
            timed_out: false,
            output_limit_exceeded: false,
            duration_ms: 90,
        }),
        graders: Vec::new(),
        submission_sha256: None,
        started_at_ms: 0,
        completed_at_ms: 100,
        duration_ms: 100,
        model_proxy: Some(ProxyRecord {
            kind: String::from("openai"),
            upstream: String::from("https://example.com"),
            allowed_paths: vec![String::from("/v1/responses")],
            max_requests: 20,
            credential_fingerprint: format!("credential-{harness}"),
        }),
        metrics: EvaluationMetrics {
            proxy: None,
            harness: Some(HarnessMetrics {
                schema_version: 1,
                turns: None,
                script_executions: None,
                final_context_tokens: None,
                usage: Some(UsageMetrics {
                    total_tokens: 42,
                    ..UsageMetrics::default()
                }),
            }),
        },
        controller_failure: None,
        provider_config_sha256: None,
        rust_environment_programs: Some(vec![String::from("/nix/store/test-rust/bin/rustc")]),
    }
}

fn rejects(result: Result<ComparisonResult>, expected: &str) -> Result<()> {
    let Err(error) = result else {
        bail!("comparison should have rejected {expected}");
    };
    ensure!(
        format!("{error:#}").contains(expected),
        "unexpected error: {error:#}"
    );
    Ok(())
}

#[test]
fn compares_paired_outcomes_and_excludes_infrastructure_failures() -> Result<()> {
    let cache = TestCache::new()?;
    let mut controller_failure = run("left", 3, RunStatus::ControllerFailed);
    controller_failure.model_proxy = None;
    let left = cache.write_suite(
        "left",
        &[
            run("left", 0, RunStatus::Passed),
            run("left", 1, RunStatus::RunnerFailed),
            run("left", 2, RunStatus::Passed),
            controller_failure,
        ],
        &[4],
    )?;
    let mut without_usage = run("right", 2, RunStatus::Passed);
    without_usage.metrics = EvaluationMetrics::default();
    let right = cache.write_suite(
        "right",
        &[
            run("right", 4, RunStatus::Passed),
            run("right", 3, RunStatus::Passed),
            without_usage,
            run("right", 1, RunStatus::Passed),
            run("right", 0, RunStatus::Failed),
        ],
        &[],
    )?;
    let comparison = compare(&left, &right)?;
    ensure!(
        comparison.paired_runs == 5
            && comparison.evaluated_pairs == 3
            && comparison.invalid_pairs == 2
    );
    ensure!(comparison.left_wins == 1 && comparison.right_wins == 1 && comparison.ties == 1);
    ensure!(comparison.left_passed == 2 && comparison.right_passed == 2);
    ensure!(comparison.left_success_rate == Some(2.0 / 3.0));
    ensure!(comparison.right_success_rate == Some(2.0 / 3.0));
    ensure!(comparison.wall_time_ms.samples == 3 && comparison.wall_time_ms.left_total == 300);
    ensure!(
        comparison.runner_time_ms.samples == 3
            && comparison.runner_time_ms.right_mean == Some(90.0)
    );
    ensure!(comparison.total_tokens.samples == 2 && comparison.total_tokens.left_total == 84);
    ensure!(
        comparison
            .runs
            .first()
            .is_some_and(|run| run.outcome == PairOutcome::LeftWin)
    );
    ensure!(
        comparison
            .runs
            .last()
            .is_some_and(|run| run.outcome == PairOutcome::Invalid)
    );
    Ok(())
}

#[test]
fn all_invalid_pairs_have_no_success_rate_or_metric_samples() -> Result<()> {
    let cache = TestCache::new()?;
    let left = cache.write_suite("left", &[], &[0])?;
    let right = cache.write_suite("right", &[], &[0])?;
    let comparison = compare(&left, &right)?;
    ensure!(comparison.invalid_pairs == 1 && comparison.evaluated_pairs == 0);
    ensure!(comparison.left_success_rate.is_none() && comparison.right_success_rate.is_none());
    ensure!(comparison.wall_time_ms.samples == 0 && comparison.total_tokens.left_mean.is_none());
    Ok(())
}

#[test]
fn rejects_duplicate_and_missing_pairs() -> Result<()> {
    let cache = TestCache::new()?;
    let first = run("left", 0, RunStatus::Passed);
    let left = cache.write_suite("left", &[first.clone(), first], &[])?;
    let right = cache.write_suite("right", &[run("right", 0, RunStatus::Passed)], &[])?;
    rejects(
        compare(&left, &right),
        "duplicate task sample-task attempt 0",
    )?;
    let left = cache.write_suite("left", &[run("left", 1, RunStatus::Passed)], &[])?;
    rejects(
        compare(&left, &right),
        "right suite is missing task sample-task attempt 1",
    )?;
    let left = cache.write_suite("left", &[run("left", 0, RunStatus::Passed)], &[])?;
    let right = cache.write_suite(
        "right",
        &[
            run("right", 0, RunStatus::Passed),
            run("right", 1, RunStatus::Passed),
        ],
        &[],
    )?;
    rejects(
        compare(&left, &right),
        "left suite is missing task sample-task attempt 1",
    )
}

#[test]
fn rejects_changed_tasks_graders_and_execution_constraints() -> Result<()> {
    let cache = TestCache::new()?;
    let left = cache.write_suite("left", &[run("left", 0, RunStatus::Passed)], &[])?;
    type Mutation = (&'static str, fn(&mut RunResult));
    let mutations: &[Mutation] = &[
        ("task hash", |run| run.task_sha256.push('x')),
        ("grader hash", |run| run.grader_sha256.push('x')),
        ("sandbox backend", |run| run.sandbox.backend.push('x')),
        ("network policy", |run| {
            run.sandbox.network = NetworkPolicy::Disabled
        }),
        ("environment policy", |run| {
            run.sandbox.environment_cleared = false
        }),
        ("memory limit", |run| run.sandbox.max_memory_bytes += 1),
        ("process limit", |run| run.sandbox.max_processes += 1),
        ("CPU limit", |run| run.sandbox.cpu_quota_percent += 1),
        ("Rust toolchain", |run| run.rust_environment_programs = None),
        ("model proxy configuration", |run| run.model_proxy = None),
        ("model proxy request limit", |run| {
            if let Some(proxy) = &mut run.model_proxy {
                proxy.max_requests += 1;
            }
        }),
        ("model proxy allowed paths", |run| {
            if let Some(proxy) = &mut run.model_proxy {
                proxy.allowed_paths.push(String::from("/other"));
            }
        }),
    ];
    for (field, mutate) in mutations {
        let mut result = run("right", 0, RunStatus::Passed);
        mutate(&mut result);
        let right = cache.write_suite("right", &[result], &[])?;
        rejects(compare(&left, &right), field)?;
    }
    Ok(())
}

#[test]
fn rejects_changed_models_and_summary_artifact_disagreement() -> Result<()> {
    let cache = TestCache::new()?;
    let left = cache.write_suite("left", &[run("left", 0, RunStatus::Passed)], &[])?;
    let right = cache.write_suite("right", &[run("right", 0, RunStatus::Passed)], &[])?;
    let mut suite = read_suite(&right)?;
    suite.model_label = Some(String::from("other-model"));
    fs::write(&right, serde_json::to_vec(&suite)?)?;
    rejects(compare(&left, &right), "different model labels")?;
    suite.model_label = Some(String::from("test-model"));
    if let Some(run) = suite.runs.first_mut() {
        run.status = Some(RunStatus::Failed);
    }
    fs::write(&right, serde_json::to_vec(&suite)?)?;
    rejects(compare(&left, &right), "disagrees with its suite entry")
}

#[test]
fn moved_summaries_can_use_explicit_independent_cache_roots() -> Result<()> {
    let left_cache = TestCache::new()?;
    let right_cache = TestCache::new()?;
    let left = left_cache.write_suite("left", &[run("left", 0, RunStatus::Passed)], &[])?;
    let right = right_cache.write_suite("right", &[run("right", 0, RunStatus::Passed)], &[])?;
    let moved = left_cache.0.join("exported-summary.json");
    fs::copy(left, &moved)?;
    rejects(compare(&moved, &right), "cannot infer cache root")?;
    let comparison = compare_with_cache_roots(&moved, &right, &left_cache.0, &right_cache.0)?;
    ensure!(comparison.evaluated_pairs == 1 && comparison.ties == 1);
    Ok(())
}

#[test]
fn rejects_missing_artifacts_and_unidentified_launch_failures() -> Result<()> {
    let cache = TestCache::new()?;
    let result = run("left", 0, RunStatus::Passed);
    let result_path = cache.0.join(&result.artifact_path).join("result.json");
    let left = cache.write_suite("left", &[result], &[])?;
    let right = cache.write_suite("right", &[run("right", 0, RunStatus::Passed)], &[])?;
    fs::remove_file(result_path)?;
    rejects(compare(&left, &right), "read run result")?;
    let left = cache.write_suite("left", &[], &[0])?;
    let mut suite = read_suite(&left)?;
    if let Some(run) = suite.runs.first_mut() {
        run.task_id = None;
    }
    fs::write(&left, serde_json::to_vec(&suite)?)?;
    rejects(compare(&left, &right), "without a task id")
}
