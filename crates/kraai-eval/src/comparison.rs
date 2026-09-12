use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail, ensure, eyre};
use serde::{Deserialize, Serialize};

use crate::suite::SuiteRunResult;
use crate::{RunResult, RunStatus, SuiteResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonResult {
    pub schema_version: u32,
    pub left: ComparisonSuite,
    pub right: ComparisonSuite,
    pub paired_runs: u64,
    pub evaluated_pairs: u64,
    pub invalid_pairs: u64,
    pub left_passed: u64,
    pub right_passed: u64,
    pub left_success_rate: Option<f64>,
    pub right_success_rate: Option<f64>,
    pub left_wins: u64,
    pub right_wins: u64,
    pub ties: u64,
    pub wall_time_ms: PairedMetric,
    pub runner_time_ms: PairedMetric,
    pub total_tokens: PairedMetric,
    pub runs: Vec<ComparedRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonSuite {
    pub suite_id: String,
    pub harness_name: String,
    pub runner_version: String,
    pub model_label: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairedMetric {
    pub samples: u64,
    pub left_total: u128,
    pub right_total: u128,
    pub left_mean: Option<f64>,
    pub right_mean: Option<f64>,
}

impl PairedMetric {
    fn record(&mut self, left: Option<u128>, right: Option<u128>) {
        if let (Some(left), Some(right)) = (left, right) {
            self.samples += 1;
            self.left_total += left;
            self.right_total += right;
            self.left_mean = Some(self.left_total as f64 / self.samples as f64);
            self.right_mean = Some(self.right_total as f64 / self.samples as f64);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparedRun {
    pub task_id: String,
    pub attempt: u64,
    pub left_status: Option<RunStatus>,
    pub right_status: Option<RunStatus>,
    pub outcome: PairOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairOutcome {
    LeftWin,
    RightWin,
    Tie,
    Invalid,
}

type RunKey = (String, u64);

struct LoadedRun {
    summary: SuiteRunResult,
    result: Option<RunResult>,
}

pub fn compare(left_summary: &Path, right_summary: &Path) -> Result<ComparisonResult> {
    let left = read_suite(left_summary)?;
    let right = read_suite(right_summary)?;
    let left_cache = infer_cache_root(left_summary, &left)?;
    let right_cache = infer_cache_root(right_summary, &right)?;
    compare_suites(left, right, &left_cache, &right_cache)
}

pub fn compare_with_cache_roots(
    left_summary: &Path,
    right_summary: &Path,
    left_cache: &Path,
    right_cache: &Path,
) -> Result<ComparisonResult> {
    compare_suites(
        read_suite(left_summary)?,
        read_suite(right_summary)?,
        left_cache,
        right_cache,
    )
}

fn compare_suites(
    left: SuiteResult,
    right: SuiteResult,
    left_cache: &Path,
    right_cache: &Path,
) -> Result<ComparisonResult> {
    ensure!(
        left.model_label == right.model_label,
        "cannot compare suites with different model labels: {:?} versus {:?}",
        left.model_label,
        right.model_label
    );
    let mut comparison = ComparisonResult {
        schema_version: 1,
        left: suite_identity(&left),
        right: suite_identity(&right),
        paired_runs: 0,
        evaluated_pairs: 0,
        invalid_pairs: 0,
        left_passed: 0,
        right_passed: 0,
        left_success_rate: None,
        right_success_rate: None,
        left_wins: 0,
        right_wins: 0,
        ties: 0,
        wall_time_ms: PairedMetric::default(),
        runner_time_ms: PairedMetric::default(),
        total_tokens: PairedMetric::default(),
        runs: Vec::new(),
    };
    let left_runs = load_runs(left, left_cache).wrap_err("load left suite")?;
    let mut right_runs = load_runs(right, right_cache).wrap_err("load right suite")?;
    for key in left_runs.keys() {
        ensure!(
            right_runs.contains_key(key),
            "right suite is missing task {} attempt {}",
            key.0,
            key.1
        );
    }
    for key in right_runs.keys() {
        ensure!(
            left_runs.contains_key(key),
            "left suite is missing task {} attempt {}",
            key.0,
            key.1
        );
    }
    for (key, left) in left_runs {
        let right = right_runs
            .remove(&key)
            .ok_or_else(|| eyre!("missing paired run for {} attempt {}", key.0, key.1))?;
        comparison.paired_runs += 1;
        if let (Some(left), Some(right)) = (&left.result, &right.result) {
            validate_pair(left, right)
                .wrap_err_with(|| format!("cannot compare task {} attempt {}", key.0, key.1))?;
        }
        let outcome = match (&left.result, &right.result) {
            (Some(left), Some(right))
                if left.status != RunStatus::ControllerFailed
                    && right.status != RunStatus::ControllerFailed =>
            {
                comparison.evaluated_pairs += 1;
                comparison
                    .wall_time_ms
                    .record(Some(left.duration_ms), Some(right.duration_ms));
                comparison.runner_time_ms.record(
                    left.runner.as_ref().map(|runner| runner.duration_ms),
                    right.runner.as_ref().map(|runner| runner.duration_ms),
                );
                comparison.total_tokens.record(
                    left.metrics
                        .usage()
                        .map(|usage| u128::from(usage.total_tokens)),
                    right
                        .metrics
                        .usage()
                        .map(|usage| u128::from(usage.total_tokens)),
                );
                let left_passed = left.status == RunStatus::Passed;
                let right_passed = right.status == RunStatus::Passed;
                comparison.left_passed += u64::from(left_passed);
                comparison.right_passed += u64::from(right_passed);
                match (left_passed, right_passed) {
                    (true, false) => {
                        comparison.left_wins += 1;
                        PairOutcome::LeftWin
                    }
                    (false, true) => {
                        comparison.right_wins += 1;
                        PairOutcome::RightWin
                    }
                    _ => {
                        comparison.ties += 1;
                        PairOutcome::Tie
                    }
                }
            }
            _ => {
                comparison.invalid_pairs += 1;
                PairOutcome::Invalid
            }
        };
        comparison.runs.push(ComparedRun {
            task_id: key.0,
            attempt: key.1,
            left_status: left.summary.status,
            right_status: right.summary.status,
            outcome,
        });
    }
    if comparison.evaluated_pairs != 0 {
        comparison.left_success_rate =
            Some(comparison.left_passed as f64 / comparison.evaluated_pairs as f64);
        comparison.right_success_rate =
            Some(comparison.right_passed as f64 / comparison.evaluated_pairs as f64);
    }
    Ok(comparison)
}

fn read_suite(path: &Path) -> Result<SuiteResult> {
    let suite: SuiteResult = serde_json::from_slice(
        &fs::read(path).wrap_err_with(|| format!("read suite summary {}", path.display()))?,
    )
    .wrap_err_with(|| format!("parse suite summary {}", path.display()))?;
    ensure!(
        suite.schema_version == 1,
        "unsupported suite schema version {}",
        suite.schema_version
    );
    ensure!(
        !suite.runs.is_empty(),
        "suite {} contains no runs",
        suite.suite_id
    );
    ensure!(
        suite.requested_runs == suite.runs.len() as u64,
        "suite {} requested run count does not match its run entries",
        suite.suite_id
    );
    Ok(suite)
}

fn infer_cache_root(summary_path: &Path, suite: &SuiteResult) -> Result<PathBuf> {
    validate_relative_path(&suite.artifact_path)?;
    let summary_path = summary_path.canonicalize()?;
    let mut root = summary_path
        .parent()
        .ok_or_else(|| eyre!("suite summary path has no parent"))?
        .to_path_buf();
    ensure!(
        root.ends_with(&suite.artifact_path),
        "cannot infer cache root for {}; use explicit cache roots for moved summaries",
        summary_path.display()
    );
    for _ in suite.artifact_path.components() {
        root.pop();
    }
    Ok(root)
}

fn load_runs(suite: SuiteResult, cache: &Path) -> Result<BTreeMap<RunKey, LoadedRun>> {
    let mut runs = BTreeMap::new();
    for summary in suite.runs {
        let task_id = summary
            .task_id
            .as_ref()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| {
                eyre!(
                    "suite {} has a run without a task id; it cannot be paired",
                    suite.suite_id
                )
            })?;
        let key = (task_id.clone(), summary.attempt);
        ensure!(
            !runs.contains_key(&key),
            "duplicate task {} attempt {}",
            key.0,
            key.1
        );
        let result = if let Some(status) = &summary.status {
            let artifact = summary.artifact_path.as_ref().ok_or_else(|| {
                eyre!(
                    "task {} attempt {} has a status but no result artifact",
                    key.0,
                    key.1
                )
            })?;
            validate_relative_path(artifact)?;
            let path = cache.join(artifact).join("result.json");
            let result: RunResult = serde_json::from_slice(
                &fs::read(&path).wrap_err_with(|| format!("read run result {}", path.display()))?,
            )
            .wrap_err_with(|| format!("parse run result {}", path.display()))?;
            ensure!(
                result.schema_version == 6,
                "unsupported run result schema version {}",
                result.schema_version
            );
            ensure!(
                result.task_id == *task_id
                    && result.attempt == summary.attempt
                    && result.status == *status
                    && summary.experiment_id.as_ref() == Some(&result.experiment_id)
                    && result.artifact_path == *artifact
                    && result.harness_name == suite.harness_name
                    && result.runner_version == suite.runner_version
                    && result.model_label == suite.model_label,
                "run result {} disagrees with its suite entry or suite identity",
                path.display()
            );
            Some(result)
        } else {
            ensure!(
                summary.artifact_path.is_none() && summary.experiment_id.is_none(),
                "task {} attempt {} references a result without a status",
                key.0,
                key.1
            );
            None
        };
        runs.insert(key, LoadedRun { summary, result });
    }
    Ok(runs)
}

fn validate_relative_path(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "artifact path must be relative and contain no parent traversal: {}",
        path.display()
    );
    Ok(())
}

fn validate_pair(left: &RunResult, right: &RunResult) -> Result<()> {
    matching(&left.task_sha256, &right.task_sha256, "task hash")?;
    matching(&left.grader_sha256, &right.grader_sha256, "grader hash")?;
    matching(&left.model_label, &right.model_label, "model label")?;
    matching(
        &left.sandbox.backend,
        &right.sandbox.backend,
        "sandbox backend",
    )?;
    matching(
        &left.sandbox.network,
        &right.sandbox.network,
        "network policy",
    )?;
    matching(
        &left.sandbox.environment_cleared,
        &right.sandbox.environment_cleared,
        "environment policy",
    )?;
    matching(
        &left.sandbox.max_memory_bytes,
        &right.sandbox.max_memory_bytes,
        "memory limit",
    )?;
    matching(
        &left.sandbox.max_processes,
        &right.sandbox.max_processes,
        "process limit",
    )?;
    matching(
        &left.sandbox.cpu_quota_percent,
        &right.sandbox.cpu_quota_percent,
        "CPU limit",
    )?;
    matching(
        &left.rust_environment_programs,
        &right.rust_environment_programs,
        "Rust toolchain",
    )?;
    if left.status == RunStatus::ControllerFailed || right.status == RunStatus::ControllerFailed {
        return Ok(());
    }
    match (&left.model_proxy, &right.model_proxy) {
        (Some(left), Some(right)) => {
            matching(&left.kind, &right.kind, "model proxy kind")?;
            matching(&left.upstream, &right.upstream, "model proxy upstream")?;
            matching(
                &left.allowed_paths.iter().collect::<BTreeSet<_>>(),
                &right.allowed_paths.iter().collect::<BTreeSet<_>>(),
                "model proxy allowed paths",
            )?;
            matching(
                &left.max_requests,
                &right.max_requests,
                "model proxy request limit",
            )?;
        }
        (None, None) => {}
        _ => bail!("model proxy configuration differs"),
    }
    Ok(())
}

fn matching<T: PartialEq>(left: &T, right: &T, field: &str) -> Result<()> {
    ensure!(left == right, "{field} differs");
    Ok(())
}

fn suite_identity(suite: &SuiteResult) -> ComparisonSuite {
    ComparisonSuite {
        suite_id: suite.suite_id.clone(),
        harness_name: suite.harness_name.clone(),
        runner_version: suite.runner_version.clone(),
        model_label: suite.model_label.clone(),
    }
}

#[cfg(test)]
mod tests;
