pub(crate) mod metrics;
mod records;

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{PairOutcome, PairedMetric};
pub use metrics::{HarborEfficiencyMetrics, HarborTrialMetrics};
use records::{HarborJob, load_job};

trait JsonField {
    fn field(&self, pointer: &str) -> &serde_json::Value;
}

impl JsonField for serde_json::Value {
    fn field(&self, pointer: &str) -> &serde_json::Value {
        self.pointer(pointer).unwrap_or(&Self::Null)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarborComparisonResult {
    pub schema_version: u32,
    pub left: HarborJobIdentity,
    pub right: HarborJobIdentity,
    pub paired_runs: u64,
    pub evaluated_pairs: u64,
    pub invalid_pairs: u64,
    pub left_passed: u64,
    pub right_passed: u64,
    pub left_wins: u64,
    pub right_wins: u64,
    pub ties: u64,
    pub both_passed: u64,
    pub all_attempts: HarborEfficiencyMetrics,
    pub both_passed_efficiency: HarborEfficiencyMetrics,
    pub wall_clock_reliable: bool,
    pub runs: Vec<HarborComparedRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarborJobIdentity {
    pub directory: PathBuf,
    pub job_id: String,
    pub harbor_version: String,
    pub harness: String,
    pub agent_version: String,
    pub configured_model: String,
    pub lock_sha256: String,
    pub result_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborModelSettings {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarborComparedRun {
    pub task_name: String,
    pub task_digest: String,
    pub attempt: u64,
    pub left: HarborTrialSummary,
    pub right: HarborTrialSummary,
    pub outcome: PairOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarborTrialSummary {
    pub directory: PathBuf,
    pub trial_name: String,
    pub task_source: Option<String>,
    pub status: HarborTrialStatus,
    pub reward: Option<f64>,
    pub exception_type: Option<String>,
    pub model_settings: Option<HarborModelSettings>,
    pub proxy_identity_sha256: Option<String>,
    pub lock_sha256: String,
    pub result_sha256: String,
    pub metrics: HarborTrialMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarborTrialStatus {
    Passed,
    Failed,
    AgentFailed,
    InfrastructureError,
}

pub fn compare_harbor_jobs(left: &Path, right: &Path) -> Result<HarborComparisonResult> {
    compare_jobs(load_job(left)?, load_job(right)?)
}

fn compare_jobs(left: HarborJob, mut right: HarborJob) -> Result<HarborComparisonResult> {
    ensure!(
        left.settings == right.settings,
        "Harbor job execution settings differ"
    );
    ensure!(
        left.identity.configured_model == right.identity.configured_model,
        "Harbor configured model differs"
    );
    ensure!(
        left.trials.keys().eq(right.trials.keys()),
        "Harbor task populations differ"
    );
    let mut result = HarborComparisonResult {
        schema_version: 1,
        left: left.identity,
        right: right.identity,
        paired_runs: 0,
        evaluated_pairs: 0,
        invalid_pairs: 0,
        left_passed: 0,
        right_passed: 0,
        left_wins: 0,
        right_wins: 0,
        ties: 0,
        both_passed: 0,
        all_attempts: HarborEfficiencyMetrics::default(),
        both_passed_efficiency: HarborEfficiencyMetrics::default(),
        wall_clock_reliable: false,
        runs: Vec::new(),
    };
    for ((task_name, task_digest), left_trials) in left.trials {
        let right_trials = right
            .trials
            .remove(&(task_name.clone(), task_digest.clone()))
            .ok_or_else(|| color_eyre::eyre::eyre!("missing Harbor task {task_name}"))?;
        ensure!(
            left_trials.len() == right_trials.len(),
            "Harbor attempt count differs for {task_name}"
        );
        for (attempt, (left, right)) in left_trials.into_iter().zip(right_trials).enumerate() {
            ensure!(
                left.settings == right.settings,
                "Harbor task execution settings differ for {task_name}"
            );
            result.paired_runs += 1;
            let left = left.summary;
            let right = right.summary;
            result.all_attempts.record(&left.metrics, &right.metrics);
            result.left_passed += u64::from(left.status == HarborTrialStatus::Passed);
            result.right_passed += u64::from(right.status == HarborTrialStatus::Passed);
            let outcome = if left.status == HarborTrialStatus::InfrastructureError
                || right.status == HarborTrialStatus::InfrastructureError
            {
                result.invalid_pairs += 1;
                PairOutcome::Invalid
            } else {
                ensure!(
                    left.model_settings.is_some() && left.model_settings == right.model_settings,
                    "Harbor actual model settings are missing or differ for {task_name}"
                );
                ensure!(
                    left.proxy_identity_sha256 == right.proxy_identity_sha256,
                    "Harbor proxy configuration differs for {task_name}"
                );
                result.evaluated_pairs += 1;
                match (
                    left.status == HarborTrialStatus::Passed,
                    right.status == HarborTrialStatus::Passed,
                ) {
                    (true, true) => {
                        result.both_passed += 1;
                        result
                            .both_passed_efficiency
                            .record(&left.metrics, &right.metrics);
                        result.ties += 1;
                        PairOutcome::Tie
                    }
                    (true, false) => {
                        result.left_wins += 1;
                        PairOutcome::LeftWin
                    }
                    (false, true) => {
                        result.right_wins += 1;
                        PairOutcome::RightWin
                    }
                    (false, false) => {
                        result.ties += 1;
                        PairOutcome::Tie
                    }
                }
            };
            result.runs.push(HarborComparedRun {
                task_name: task_name.clone(),
                task_digest: task_digest.clone(),
                attempt: attempt as u64,
                left,
                right,
                outcome,
            });
        }
    }
    Ok(result)
}

pub fn format_harbor_comparison(result: &HarborComparisonResult) -> String {
    let mut output = format!(
        "Left: {} @ {}\nRight: {} @ {}\nHarbor: {}\nConfigured model: {}\nPlanned attempts: {} per harness\nPassed: {} left, {} right; errors count toward planned attempts\nPairs: {} evaluated, {} invalid\nWins: {} left, {} right; {} ties\n\nAll planned attempts:",
        result.left.harness,
        result.left.agent_version,
        result.right.harness,
        result.right.agent_version,
        result.left.harbor_version,
        result.left.configured_model,
        result.paired_runs,
        result.left_passed,
        result.right_passed,
        result.evaluated_pairs,
        result.invalid_pairs,
        result.left_wins,
        result.right_wins,
        result.ties,
    );
    append_metrics(&mut output, &result.all_attempts);
    output.push_str(&format!(
        "\n\nBoth harnesses passed: {} pairs",
        result.both_passed
    ));
    append_metrics(&mut output, &result.both_passed_efficiency);
    output.push_str("\n\nAttempts are paired by recorded start order within each task.\nTiming is observational; shared-machine load can affect wall and runner time.");
    for run in &result.runs {
        output.push_str(&format!(
            "\n{} attempt {}: {:?} left, {:?} right; {:?}",
            run.task_name, run.attempt, run.left.status, run.right.status, run.outcome
        ));
    }
    output
}

fn append_metrics(output: &mut String, metrics: &HarborEfficiencyMetrics) {
    output.push_str(&metrics.request_metrics.display());
    for (name, metric) in metrics.rows() {
        output.push_str(&format!(
            "\n{name}: {} left mean, {} right mean, {} measured pairs",
            format_mean(metric.left_mean),
            format_mean(metric.right_mean),
            metric.samples
        ));
    }
}

fn format_mean(value: Option<f64>) -> String {
    value.map_or_else(|| String::from("n/a"), |value| format!("{value:.1}"))
}

#[cfg(test)]
mod tests;
