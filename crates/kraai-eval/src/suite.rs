use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{RunRequest, RunResult, RunStatus, UsageMetrics};

#[derive(Debug, Clone)]
pub struct SuiteRequest {
    pub runs: Vec<RunRequest>,
    pub eval_root: PathBuf,
    pub harness_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteResult {
    pub schema_version: u32,
    pub artifact_path: PathBuf,
    pub harness_name: String,
    pub runner_version: String,
    pub model_label: Option<String>,
    pub started_at_ms: u128,
    pub completed_at_ms: u128,
    pub duration_ms: u128,
    pub requested_runs: u64,
    pub evaluated_runs: u64,
    pub passed_runs: u64,
    pub failed_runs: u64,
    pub controller_failures: u64,
    pub launch_failures: u64,
    pub success_rate: Option<f64>,
    pub wall_time_ms: Distribution,
    pub total_tokens: TokenSummary,
    pub used_context_tokens: TokenSummary,
    pub runs: Vec<SuiteRunResult>,
}

#[derive(Debug, Clone)]
pub struct SuiteOutcome {
    pub summary: SuiteResult,
    pub results: Vec<RunResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Distribution {
    pub samples: u64,
    pub min: Option<u128>,
    pub max: Option<u128>,
    pub mean: Option<f64>,
    pub p50: Option<u128>,
    pub p95: Option<u128>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenSummary {
    pub total: u128,
    pub distribution: Distribution,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteRunResult {
    pub task_id: Option<String>,
    pub attempt: u64,
    pub status: Option<RunStatus>,
    pub experiment_id: Option<String>,
    pub artifact_path: Option<PathBuf>,
    pub duration_ms: Option<u128>,
    pub usage: Option<UsageMetrics>,
    #[serde(default)]
    pub accounting: Option<crate::AccountingSummary>,
    pub error: Option<String>,
}

pub fn run_suite(request: &SuiteRequest) -> Result<SuiteOutcome> {
    if request.runs.is_empty() {
        bail!("suite must contain at least one run");
    }
    let started = Instant::now();
    let started_at_ms = unix_timestamp_ms()?;
    let first_run = request.runs.first();
    let harness_name = first_run
        .map(RunRequest::resolved_harness_name)
        .unwrap_or_else(|| String::from("unnamed-harness"));
    let runner_version = first_run
        .map(|run| run.runner_version.as_str())
        .unwrap_or("unversioned")
        .to_owned();
    let model_label = first_run.and_then(RunRequest::resolved_model_label);
    if request.runs.iter().any(|run| {
        run.resolved_harness_name() != harness_name
            || run.runner_version != runner_version
            || run.resolved_model_label() != model_label
    }) {
        bail!("suite runs must use one harness, runner version, and model label");
    }
    if request.runs.iter().any(|run| {
        run.eval_root != request.eval_root || !run.artifact_path.starts_with(&request.harness_dir)
    }) {
        bail!("suite runs must save their attempts inside the suite's harness folder");
    }
    let artifact_dir = request.eval_root.join(&request.harness_dir);
    fs::create_dir_all(&artifact_dir)?;
    let (saved, _) = saved_results(&request.eval_root, &request.harness_dir)?;
    if let Some(other) = saved
        .iter()
        .find(|result| result.harness_name != harness_name || result.model_label != model_label)
    {
        bail!(
            "{} holds attempts by {} on {}, not {harness_name} on {}; their names map to the same results folder",
            request.harness_dir.display(),
            other.harness_name,
            other.model_label.as_deref().unwrap_or("no model"),
            model_label.as_deref().unwrap_or("no model"),
        );
    }

    let mut launch_failures = Vec::new();
    let mut failed_attempts = BTreeSet::new();
    for run_request in &request.runs {
        if let Err(error) = crate::run(run_request) {
            failed_attempts.insert(run_request.artifact_path.clone());
            launch_failures.push(SuiteRunResult {
                task_id: crate::TaskManifest::load(&run_request.task_path)
                    .ok()
                    .map(|task| task.id),
                attempt: run_request.attempt,
                status: None,
                experiment_id: None,
                artifact_path: None,
                duration_ms: None,
                usage: None,
                accounting: None,
                error: Some(format!("{error:#}")),
            });
        }
    }
    let (results, unreadable) = saved_results(&request.eval_root, &request.harness_dir)?;
    launch_failures.extend(
        unreadable
            .into_iter()
            .filter(|attempt| !failed_attempts.contains(&attempt.artifact_path))
            .map(|attempt| SuiteRunResult {
                task_id: Some(attempt.task),
                attempt: attempt.attempt,
                status: None,
                experiment_id: None,
                artifact_path: Some(attempt.artifact_path),
                duration_ms: None,
                usage: None,
                accounting: None,
                error: Some(attempt.error),
            }),
    );
    let mut runs = results
        .iter()
        .map(|result| SuiteRunResult {
            task_id: Some(result.task_id.clone()),
            attempt: result.attempt,
            status: Some(result.status.clone()),
            experiment_id: Some(result.experiment_id.clone()),
            artifact_path: Some(result.artifact_path.clone()),
            duration_ms: Some(result.duration_ms),
            usage: result.metrics.usage().cloned(),
            accounting: result
                .metrics
                .proxy
                .as_ref()
                .and_then(|proxy| proxy.accounting.as_ref())
                .map(crate::RequestAccounting::summary),
            error: result
                .controller_failure
                .as_ref()
                .map(|failure| failure.error.clone()),
        })
        .chain(launch_failures)
        .collect::<Vec<_>>();
    runs.sort_by(|left, right| {
        left.task_id
            .cmp(&right.task_id)
            .then(left.attempt.cmp(&right.attempt))
    });

    let evaluated_runs = count_statuses(&runs, |status| status != &RunStatus::ControllerFailed);
    let passed_runs = count_statuses(&runs, |status| status == &RunStatus::Passed);
    let controller_failures =
        count_statuses(&runs, |status| status == &RunStatus::ControllerFailed);
    let launch_failures = runs.iter().filter(|run| run.status.is_none()).count() as u64;
    let failed_runs = evaluated_runs.saturating_sub(passed_runs);
    let wall_times = runs
        .iter()
        .filter_map(|run| run.duration_ms)
        .collect::<Vec<_>>();
    let total_tokens = token_summary(
        runs.iter()
            .filter_map(|run| {
                run.usage
                    .as_ref()
                    .map(|usage| u128::from(usage.total_tokens))
            })
            .collect(),
    );
    let used_context_tokens = token_summary(
        runs.iter()
            .filter_map(|run| {
                run.usage
                    .as_ref()
                    .map(|usage| u128::from(usage.used_context_tokens()))
            })
            .collect(),
    );
    let summary = SuiteResult {
        schema_version: 2,
        artifact_path: request.harness_dir.clone(),
        harness_name,
        runner_version,
        model_label,
        started_at_ms,
        completed_at_ms: unix_timestamp_ms()?,
        duration_ms: started.elapsed().as_millis(),
        requested_runs: runs.len() as u64,
        evaluated_runs,
        passed_runs,
        failed_runs,
        controller_failures,
        launch_failures,
        success_rate: (evaluated_runs != 0).then(|| passed_runs as f64 / evaluated_runs as f64),
        wall_time_ms: distribution(wall_times),
        total_tokens,
        used_context_tokens,
        runs,
    };
    fs::write(
        artifact_dir.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )
    .wrap_err("write suite summary")?;
    Ok(SuiteOutcome { summary, results })
}

struct UnreadableAttempt {
    task: String,
    attempt: u64,
    artifact_path: PathBuf,
    error: String,
}

fn saved_results(
    root: &Path,
    harness_dir: &Path,
) -> Result<(Vec<RunResult>, Vec<UnreadableAttempt>)> {
    let mut results = Vec::new();
    let mut unreadable = Vec::new();
    for task in fs::read_dir(root.join(harness_dir))? {
        let task = task?;
        if !task.file_type()?.is_dir() {
            continue;
        }
        for attempt in fs::read_dir(task.path())? {
            let attempt = attempt?;
            let name = attempt.file_name().to_string_lossy().into_owned();
            let path = attempt.path().join("result.json");
            let Some(number) = name.strip_prefix("attempt-") else {
                continue;
            };
            if !path.is_file() {
                continue;
            }
            match crate::load_run_result(&path) {
                Ok(result) => results.push(result),
                Err(error) => unreadable.push(UnreadableAttempt {
                    task: task.file_name().to_string_lossy().into_owned(),
                    attempt: number.parse().unwrap_or_default(),
                    artifact_path: harness_dir.join(task.file_name()).join(&name),
                    error: format!("{error:#}"),
                }),
            }
        }
    }
    Ok((results, unreadable))
}

fn count_statuses(runs: &[SuiteRunResult], predicate: impl Fn(&RunStatus) -> bool) -> u64 {
    runs.iter()
        .filter_map(|run| run.status.as_ref())
        .filter(|status| predicate(status))
        .count() as u64
}

fn token_summary(values: Vec<u128>) -> TokenSummary {
    TokenSummary {
        total: values.iter().copied().sum(),
        distribution: distribution(values),
    }
}

fn distribution(mut values: Vec<u128>) -> Distribution {
    if values.is_empty() {
        return Distribution::default();
    }
    values.sort_unstable();
    let samples = values.len() as u64;
    let total = values.iter().copied().sum::<u128>();
    Distribution {
        samples,
        min: values.first().copied(),
        max: values.last().copied(),
        mean: Some(total as f64 / samples as f64),
        p50: percentile(&values, 50),
        p95: percentile(&values, 95),
    }
}

fn percentile(values: &[u128], percentile: usize) -> Option<u128> {
    let rank = values.len().saturating_mul(percentile).saturating_add(99) / 100;
    values.get(rank.saturating_sub(1)).copied()
}

fn unix_timestamp_ms() -> Result<u128> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_uses_nearest_rank_percentiles() {
        let summary = distribution(vec![50, 10, 30, 20, 40]);
        assert_eq!(summary.samples, 5);
        assert_eq!(summary.min, Some(10));
        assert_eq!(summary.max, Some(50));
        assert_eq!(summary.mean, Some(30.0));
        assert_eq!(summary.p50, Some(30));
        assert_eq!(summary.p95, Some(50));
    }
}
