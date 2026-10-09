use std::collections::BTreeMap;

use color_eyre::eyre::{Result, ensure};

use super::{
    ChangedWorkload, Comparison, MetricComparison, Metrics, Report, Statistics, WorkloadComparison,
    WorkloadReport,
};

pub fn compare(
    baseline: &Report,
    candidate: &Report,
    max_regression_percent: Option<f64>,
) -> Result<Comparison> {
    baseline.validate()?;
    candidate.validate()?;
    if let Some(threshold) = max_regression_percent {
        ensure!(
            threshold.is_finite() && threshold >= 0.0,
            "maximum regression percent must be finite and nonnegative"
        );
    }
    ensure!(
        baseline.metadata.measurement_schema == candidate.metadata.measurement_schema,
        "reports use different measurement schemas"
    );
    ensure!(
        baseline.metadata.host == candidate.metadata.host,
        "reports were measured on different host configurations"
    );
    ensure!(
        baseline.metadata.build == candidate.metadata.build,
        "reports use different build configurations"
    );
    ensure!(
        baseline.metadata.warmups == candidate.metadata.warmups,
        "reports use different warmup counts"
    );
    let mut candidates: BTreeMap<_, _> = candidate
        .workloads
        .iter()
        .map(|workload| (workload.spec.name.as_str(), workload))
        .collect();
    let mut workloads = Vec::new();
    let mut removed_workloads = Vec::new();
    let mut changed_workloads = Vec::new();
    for original in &baseline.workloads {
        let Some(updated) = candidates.remove(original.spec.name.as_str()) else {
            removed_workloads.push(original.spec.clone());
            continue;
        };
        if original.spec != updated.spec {
            changed_workloads.push(ChangedWorkload {
                baseline: original.spec.clone(),
                candidate: updated.spec.clone(),
            });
            continue;
        }
        workloads.push(compare_workload(original, updated, max_regression_percent));
    }
    let added_workloads = candidates
        .into_values()
        .map(|workload| workload.spec.clone())
        .collect();
    let regressed = workloads.iter().any(|workload| {
        workload.metrics.wall_ns.regressed
            || workload.metrics.cpu_ns.regressed
            || workload.metrics.peak_rss_bytes.regressed
            || workload.metrics.children_peak_rss_bytes.regressed
    });
    Ok(Comparison {
        baseline_provenance: baseline.metadata.provenance.clone(),
        candidate_provenance: candidate.metadata.provenance.clone(),
        max_regression_percent,
        regressed,
        workloads,
        added_workloads,
        removed_workloads,
        changed_workloads,
    })
}

fn compare_workload(
    baseline: &WorkloadReport,
    candidate: &WorkloadReport,
    max_regression_percent: Option<f64>,
) -> WorkloadComparison {
    let before = &baseline.summary;
    let after = &candidate.summary;
    WorkloadComparison {
        spec: baseline.spec.clone(),
        metrics: Metrics {
            wall_ns: compare_metric(&before.wall_ns, &after.wall_ns, max_regression_percent),
            user_cpu_ns: compare_metric(&before.user_cpu_ns, &after.user_cpu_ns, None),
            system_cpu_ns: compare_metric(&before.system_cpu_ns, &after.system_cpu_ns, None),
            children_user_cpu_ns: compare_metric(
                &before.children_user_cpu_ns,
                &after.children_user_cpu_ns,
                None,
            ),
            children_system_cpu_ns: compare_metric(
                &before.children_system_cpu_ns,
                &after.children_system_cpu_ns,
                None,
            ),
            cpu_ns: compare_metric(&before.cpu_ns, &after.cpu_ns, max_regression_percent),
            peak_rss_bytes: compare_metric(
                &before.peak_rss_bytes,
                &after.peak_rss_bytes,
                max_regression_percent,
            ),
            children_peak_rss_bytes: compare_metric(
                &before.children_peak_rss_bytes,
                &after.children_peak_rss_bytes,
                max_regression_percent,
            ),
        },
    }
}

fn compare_metric(
    baseline: &Statistics,
    candidate: &Statistics,
    max_regression_percent: Option<f64>,
) -> MetricComparison {
    let change_percent = if baseline.median == 0.0 {
        (candidate.median == 0.0).then_some(0.0)
    } else {
        Some((candidate.median - baseline.median) / baseline.median * 100.0)
    };
    let regressed = max_regression_percent.is_some_and(|threshold| {
        change_percent.map_or(candidate.median > baseline.median, |change| {
            change > threshold
        })
    });
    MetricComparison {
        baseline_median: baseline.median,
        candidate_median: candidate.median,
        change_percent,
        regressed,
    }
}
