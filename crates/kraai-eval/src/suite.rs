use std::fs;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{RunRequest, RunStatus, UsageMetrics};

#[derive(Debug, Clone)]
pub struct SuiteRequest {
    pub runs: Vec<RunRequest>,
    pub output_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteResult {
    pub schema_version: u32,
    pub suite_id: String,
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
    pub error: Option<String>,
}

pub fn run_suite(request: &SuiteRequest) -> Result<SuiteResult> {
    if request.runs.is_empty() {
        bail!("suite must contain at least one run");
    }
    let started = Instant::now();
    let started_at_ms = unix_timestamp_ms()?;
    let suite_id = ulid::Ulid::generate().to_string();
    let first_run = request.runs.first();
    let harness_name = first_run
        .and_then(|run| run.harness_name.as_deref())
        .or_else(|| {
            first_run
                .and_then(|run| run.runner_program.file_name())
                .and_then(|name| name.to_str())
        })
        .unwrap_or("unnamed-harness")
        .to_owned();
    let runner_version = first_run
        .map(|run| run.runner_version.as_str())
        .unwrap_or("unversioned")
        .to_owned();
    let model_label = first_run.and_then(|run| run.model_label.clone());
    if request.runs.iter().any(|run| {
        resolved_harness_name(run) != harness_name
            || run.runner_version != runner_version
            || run.model_label != model_label
    }) {
        bail!("suite runs must use one harness, runner version, and model label");
    }
    let artifact_path = PathBuf::from("suites")
        .join(crate::cache::path_segment(&harness_name, "unnamed-harness"))
        .join(crate::cache::path_segment(&runner_version, "unversioned"))
        .join(crate::cache::path_segment(
            model_label.as_deref().unwrap_or("unlabeled-model"),
            "unlabeled-model",
        ))
        .join(&suite_id);
    let artifact_dir = request.output_root.join(&artifact_path);
    fs::create_dir_all(&artifact_dir)?;

    let mut runs = Vec::with_capacity(request.runs.len());
    for run_request in &request.runs {
        match crate::run(run_request) {
            Ok(result) => runs.push(SuiteRunResult {
                task_id: Some(result.task_id),
                attempt: result.attempt,
                status: Some(result.status),
                experiment_id: Some(result.experiment_id),
                artifact_path: Some(result.artifact_path),
                duration_ms: Some(result.duration_ms),
                usage: result.metrics.usage().cloned(),
                error: result.controller_failure.map(|failure| failure.error),
            }),
            Err(error) => runs.push(SuiteRunResult {
                task_id: None,
                attempt: run_request.attempt,
                status: None,
                experiment_id: None,
                artifact_path: None,
                duration_ms: None,
                usage: None,
                error: Some(format!("{error:#}")),
            }),
        }
    }

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
    let result = SuiteResult {
        schema_version: 1,
        suite_id,
        artifact_path,
        harness_name,
        runner_version,
        model_label,
        started_at_ms,
        completed_at_ms: unix_timestamp_ms()?,
        duration_ms: started.elapsed().as_millis(),
        requested_runs: request.runs.len() as u64,
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
        serde_json::to_vec_pretty(&result)?,
    )
    .wrap_err("write suite summary")?;
    Ok(result)
}

fn resolved_harness_name(run: &RunRequest) -> String {
    run.harness_name.clone().unwrap_or_else(|| {
        run.runner_program.file_name().map_or_else(
            || String::from("unnamed-harness"),
            |name| name.to_string_lossy().into_owned(),
        )
    })
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
