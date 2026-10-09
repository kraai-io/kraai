use std::collections::{BTreeMap, BTreeSet};

use color_eyre::eyre::{Result, ensure};
use serde::{Deserialize, Serialize};

mod comparison;

pub use comparison::compare;

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub wall_ns: u64,
    pub user_cpu_ns: u64,
    pub system_cpu_ns: u64,
    pub children_user_cpu_ns: u64,
    pub children_system_cpu_ns: u64,
    pub peak_rss_bytes: u64,
    pub children_peak_rss_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub measurement_schema: String,
    pub host: BTreeMap<String, String>,
    pub build: BTreeMap<String, String>,
    pub provenance: BTreeMap<String, String>,
    pub warmups: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadSpec {
    pub name: String,
    pub fingerprint: String,
    pub operations: u64,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metrics<T> {
    pub wall_ns: T,
    pub user_cpu_ns: T,
    pub system_cpu_ns: T,
    pub children_user_cpu_ns: T,
    pub children_system_cpu_ns: T,
    pub cpu_ns: T,
    pub peak_rss_bytes: T,
    pub children_peak_rss_bytes: T,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Statistics {
    pub min: f64,
    pub max: f64,
    pub median: f64,
    pub mad: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadReport {
    pub spec: WorkloadSpec,
    pub samples: Vec<Sample>,
    pub summary: Metrics<Statistics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema_version: u32,
    pub metadata: Metadata,
    pub workloads: Vec<WorkloadReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricComparison {
    pub baseline_median: f64,
    pub candidate_median: f64,
    pub change_percent: Option<f64>,
    pub regressed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadComparison {
    pub spec: WorkloadSpec,
    pub metrics: Metrics<MetricComparison>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedWorkload {
    pub baseline: WorkloadSpec,
    pub candidate: WorkloadSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comparison {
    pub baseline_provenance: BTreeMap<String, String>,
    pub candidate_provenance: BTreeMap<String, String>,
    pub max_regression_percent: Option<f64>,
    pub regressed: bool,
    pub workloads: Vec<WorkloadComparison>,
    pub added_workloads: Vec<WorkloadSpec>,
    pub removed_workloads: Vec<WorkloadSpec>,
    pub changed_workloads: Vec<ChangedWorkload>,
}

impl Report {
    pub fn new(metadata: Metadata, workloads: Vec<WorkloadReport>) -> Result<Self> {
        let report = Self {
            schema_version: SCHEMA_VERSION,
            metadata,
            workloads,
        };
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == SCHEMA_VERSION,
            "unsupported performance report schema version {}",
            self.schema_version
        );
        ensure!(
            !self.metadata.measurement_schema.trim().is_empty(),
            "performance report has no measurement schema"
        );
        ensure!(!self.metadata.host.is_empty(), "missing host metadata");
        ensure!(!self.metadata.build.is_empty(), "missing build metadata");
        ensure!(!self.workloads.is_empty(), "report has no workloads");
        let mut names = BTreeSet::new();
        for workload in &self.workloads {
            ensure!(
                names.insert(&workload.spec.name),
                "duplicate workload {}",
                workload.spec.name
            );
            workload.validate()?;
        }
        Ok(())
    }
}

impl WorkloadReport {
    pub fn new(spec: WorkloadSpec, samples: Vec<Sample>) -> Result<Self> {
        let summary = summarize(&samples)?;
        let report = Self {
            spec,
            samples,
            summary,
        };
        report.validate()?;
        Ok(report)
    }

    fn validate(&self) -> Result<()> {
        ensure!(!self.spec.name.trim().is_empty(), "empty workload name");
        ensure!(
            !self.spec.fingerprint.trim().is_empty(),
            "workload {} has no fixture fingerprint",
            self.spec.name
        );
        ensure!(
            self.spec.operations > 0,
            "workload {} has no operations",
            self.spec.name
        );
        ensure!(
            self.summary == summarize(&self.samples)?,
            "workload {} summary does not match its samples",
            self.spec.name
        );
        Ok(())
    }
}

fn summarize(samples: &[Sample]) -> Result<Metrics<Statistics>> {
    ensure!(!samples.is_empty(), "workload has no samples");
    let cpu_ns = samples
        .iter()
        .map(|sample| {
            sample
                .user_cpu_ns
                .checked_add(sample.system_cpu_ns)
                .and_then(|total| total.checked_add(sample.children_user_cpu_ns))
                .and_then(|total| total.checked_add(sample.children_system_cpu_ns))
                .ok_or_else(|| color_eyre::eyre::eyre!("sample CPU time overflows u64"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Metrics {
        wall_ns: statistics(samples.iter().map(|sample| sample.wall_ns))?,
        user_cpu_ns: statistics(samples.iter().map(|sample| sample.user_cpu_ns))?,
        system_cpu_ns: statistics(samples.iter().map(|sample| sample.system_cpu_ns))?,
        children_user_cpu_ns: statistics(samples.iter().map(|sample| sample.children_user_cpu_ns))?,
        children_system_cpu_ns: statistics(
            samples.iter().map(|sample| sample.children_system_cpu_ns),
        )?,
        cpu_ns: statistics(cpu_ns.into_iter())?,
        peak_rss_bytes: statistics(samples.iter().map(|sample| sample.peak_rss_bytes))?,
        children_peak_rss_bytes: statistics(
            samples.iter().map(|sample| sample.children_peak_rss_bytes),
        )?,
    })
}

fn statistics(values: impl Iterator<Item = u64>) -> Result<Statistics> {
    let mut sorted: Vec<f64> = values.map(|value| value as f64).collect();
    sorted.sort_unstable_by(f64::total_cmp);
    let Some((&min, &max)) = sorted.first().zip(sorted.last()) else {
        color_eyre::eyre::bail!("cannot summarize empty measurements");
    };
    let middle_value = median(&sorted)?;
    let mut deviations: Vec<_> = sorted
        .iter()
        .map(|value| (value - middle_value).abs())
        .collect();
    deviations.sort_unstable_by(f64::total_cmp);
    Ok(Statistics {
        min,
        max,
        median: middle_value,
        mad: median(&deviations)?,
    })
}

fn median(sorted: &[f64]) -> Result<f64> {
    let middle = sorted.len() / 2;
    let value = sorted
        .get(middle)
        .ok_or_else(|| color_eyre::eyre::eyre!("cannot find median of empty measurements"))?;
    if sorted.len().is_multiple_of(2) {
        let previous = sorted
            .get(middle - 1)
            .ok_or_else(|| color_eyre::eyre::eyre!("missing lower median measurement"))?;
        Ok((previous + value) / 2.0)
    } else {
        Ok(*value)
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
