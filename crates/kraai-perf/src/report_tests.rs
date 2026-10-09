use super::*;

fn sample(wall_ns: u64, user_cpu_ns: u64, system_cpu_ns: u64, peak_rss_bytes: u64) -> Sample {
    Sample {
        wall_ns,
        user_cpu_ns,
        system_cpu_ns,
        children_user_cpu_ns: 0,
        children_system_cpu_ns: 0,
        peak_rss_bytes,
        children_peak_rss_bytes: 0,
    }
}

fn workload(samples: Vec<Sample>) -> Result<WorkloadReport> {
    WorkloadReport::new(
        WorkloadSpec {
            name: "fixture".into(),
            fingerprint: "fixture-v1".into(),
            operations: 10,
            parameters: serde_json::json!({"bytes": 1024}),
        },
        samples,
    )
}

fn report(samples: Vec<Sample>) -> Result<Report> {
    Report::new(
        Metadata {
            measurement_schema: "process-v1".into(),
            host: BTreeMap::from([("cpu".into(), "test".into())]),
            build: BTreeMap::from([("profile".into(), "release".into())]),
            provenance: BTreeMap::from([("revision".into(), "baseline".into())]),
            warmups: 1,
        },
        vec![workload(samples)?],
    )
}

fn first_workload(report: &Report) -> Result<&WorkloadReport> {
    report
        .workloads
        .first()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing test workload"))
}

fn first_workload_mut(report: &mut Report) -> Result<&mut WorkloadReport> {
    report
        .workloads
        .first_mut()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing test workload"))
}

fn first_metrics(comparison: &Comparison) -> Result<&Metrics<MetricComparison>> {
    comparison
        .workloads
        .first()
        .map(|workload| &workload.metrics)
        .ok_or_else(|| color_eyre::eyre::eyre!("missing test comparison"))
}

#[test]
fn computes_medians_and_median_absolute_deviation() -> Result<()> {
    let result = workload(vec![
        sample(100, 8, 1, 2048),
        sample(10, 1, 8, 4096),
        sample(12, 3, 3, 1024),
        sample(14, 2, 2, 8192),
    ])?;
    ensure!(
        result.summary.wall_ns
            == Statistics {
                min: 10.0,
                max: 100.0,
                median: 13.0,
                mad: 2.0,
            }
    );
    ensure!(result.summary.cpu_ns.median == 7.5);
    ensure!(result.summary.peak_rss_bytes.median == 3072.0);
    let odd = workload(vec![
        sample(20, 0, 0, 0),
        sample(1, 0, 0, 0),
        sample(4, 0, 0, 0),
    ])?;
    ensure!(odd.summary.wall_ns.median == 4.0);
    ensure!(odd.summary.wall_ns.mad == 3.0);
    Ok(())
}

#[test]
fn total_cpu_includes_children_and_memory_peaks_stay_separate() -> Result<()> {
    let mut measured = sample(100, 10, 20, 4096);
    measured.children_user_cpu_ns = 30;
    measured.children_system_cpu_ns = 40;
    measured.children_peak_rss_bytes = 8192;
    let result = workload(vec![measured])?;
    ensure!(result.summary.cpu_ns.median == 100.0);
    ensure!(result.summary.user_cpu_ns.median == 10.0);
    ensure!(result.summary.system_cpu_ns.median == 20.0);
    ensure!(result.summary.children_user_cpu_ns.median == 30.0);
    ensure!(result.summary.children_system_cpu_ns.median == 40.0);
    ensure!(result.summary.peak_rss_bytes.median == 4096.0);
    ensure!(result.summary.children_peak_rss_bytes.median == 8192.0);
    Ok(())
}

#[test]
fn round_trip_preserves_report_validation() -> Result<()> {
    let original = report(vec![sample(100, 20, 30, 4096)])?;
    let json = serde_json::to_string(&original)?;
    let restored: Report = serde_json::from_str(&json)?;
    restored.validate()?;
    ensure!(first_workload(&restored)?.summary == first_workload(&original)?.summary);
    Ok(())
}

#[test]
fn compares_medians_without_gating_cpu_components() -> Result<()> {
    let before = report(vec![sample(100, 10, 90, 1000)])?;
    let mut updated = sample(110, 90, 0, 900);
    updated.children_system_cpu_ns = 10;
    let mut after = report(vec![updated])?;
    after
        .metadata
        .provenance
        .insert("revision".into(), "candidate".into());
    let result = compare(&before, &after, Some(10.0))?;
    let metrics = first_metrics(&result)?;
    ensure!(metrics.wall_ns.change_percent == Some(10.0));
    ensure!(metrics.peak_rss_bytes.change_percent == Some(-10.0));
    ensure!(metrics.cpu_ns.change_percent == Some(0.0));
    ensure!(metrics.user_cpu_ns.change_percent == Some(800.0));
    ensure!(!metrics.user_cpu_ns.regressed);
    ensure!(!metrics.children_system_cpu_ns.regressed);
    ensure!(!result.regressed);
    ensure!(compare(&before, &after, Some(9.0))?.regressed);
    ensure!(!compare(&before, &after, None)?.regressed);
    Ok(())
}

#[test]
fn gates_total_cpu_and_both_memory_peaks() -> Result<()> {
    let mut baseline = sample(100, 10, 10, 1000);
    baseline.children_user_cpu_ns = 10;
    baseline.children_system_cpu_ns = 10;
    baseline.children_peak_rss_bytes = 1000;
    let before = report(vec![baseline.clone()])?;
    let mutations: [fn(&mut Sample); 3] = [
        |sample| sample.children_user_cpu_ns += 10,
        |sample| sample.peak_rss_bytes += 200,
        |sample| sample.children_peak_rss_bytes += 200,
    ];
    for mutate in mutations {
        let mut updated = baseline.clone();
        mutate(&mut updated);
        let after = report(vec![updated])?;
        ensure!(compare(&before, &after, Some(10.0))?.regressed);
    }
    Ok(())
}

#[test]
fn zero_baselines_never_produce_infinite_percentages() -> Result<()> {
    let before = report(vec![sample(0, 0, 0, 0)])?;
    let after = report(vec![sample(1, 0, 1, 0)])?;
    let result = compare(&before, &after, Some(100.0))?;
    ensure!(result.regressed);
    let metrics = first_metrics(&result)?;
    ensure!(metrics.wall_ns.change_percent.is_none());
    ensure!(metrics.peak_rss_bytes.change_percent == Some(0.0));
    let json = serde_json::to_value(&result)?;
    ensure!(
        json.pointer("/workloads/0/metrics/wall_ns/change_percent")
            == Some(&serde_json::Value::Null)
    );
    Ok(())
}

#[test]
fn rejects_invalid_regression_budgets() -> Result<()> {
    let original = report(vec![sample(100, 20, 30, 4096)])?;
    for threshold in [-1.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        ensure!(compare(&original, &original, Some(threshold)).is_err());
    }
    Ok(())
}

#[test]
fn rejects_incompatible_metadata() -> Result<()> {
    let original = report(vec![sample(100, 20, 30, 4096)])?;
    let mutations: [fn(&mut Metadata); 4] = [
        |metadata| metadata.measurement_schema = "different".into(),
        |metadata| {
            metadata.host.insert("cpu".into(), "different".into());
        },
        |metadata| {
            metadata.build.insert("profile".into(), "debug".into());
        },
        |metadata| metadata.warmups += 1,
    ];
    for mutate in mutations {
        let mut candidate = original.clone();
        mutate(&mut candidate.metadata);
        candidate.validate()?;
        ensure!(compare(&original, &candidate, None).is_err());
    }
    Ok(())
}

#[test]
fn classifies_changed_fixtures_without_emitting_metric_deltas() -> Result<()> {
    let original = report(vec![sample(100, 20, 30, 4096)])?;
    let mutations: [fn(&mut WorkloadSpec); 4] = [
        |spec| spec.fingerprint = "fixture-v2".into(),
        |spec| spec.operations += 1,
        |spec| spec.parameters = serde_json::json!({"bytes": 2048}),
        |spec| spec.parameters = serde_json::json!({"bytes": 1024, "description": "changed"}),
    ];
    for mutate in mutations {
        let mut candidate = report(vec![sample(1000, 200, 300, 40960)])?;
        mutate(&mut first_workload_mut(&mut candidate)?.spec);
        candidate.validate()?;
        let comparison = compare(&original, &candidate, Some(0.0))?;
        ensure!(comparison.workloads.is_empty());
        ensure!(comparison.added_workloads.is_empty());
        ensure!(comparison.removed_workloads.is_empty());
        ensure!(comparison.changed_workloads.len() == 1);
        ensure!(!comparison.regressed);
        let changed = comparison
            .changed_workloads
            .first()
            .ok_or_else(|| color_eyre::eyre::eyre!("missing changed fixture"))?;
        ensure!(changed.baseline == first_workload(&original)?.spec);
        ensure!(changed.candidate == first_workload(&candidate)?.spec);
        let serialized = serde_json::to_value(&comparison)?;
        ensure!(serialized.pointer("/changed_workloads/0/metrics").is_none());
    }
    Ok(())
}

#[test]
fn rejects_malformed_reports() -> Result<()> {
    let original = report(vec![sample(100, 20, 30, 4096)])?;
    let mutations: [fn(&mut Report); 5] = [
        |report| report.schema_version += 1,
        |report| report.metadata.measurement_schema.clear(),
        |report| report.metadata.host.clear(),
        |report| report.metadata.build.clear(),
        |report| report.workloads.clear(),
    ];
    for mutate in mutations {
        let mut invalid = original.clone();
        mutate(&mut invalid);
        ensure!(invalid.validate().is_err());
        ensure!(compare(&original, &invalid, None).is_err());
    }
    let mut duplicate = original.clone();
    duplicate.workloads.push(first_workload(&original)?.clone());
    ensure!(duplicate.validate().is_err());
    Ok(())
}

#[test]
fn rejects_malformed_workloads() -> Result<()> {
    let original = report(vec![sample(100, 20, 30, 4096)])?;
    let mutations: [fn(&mut WorkloadReport); 8] = [
        |workload| workload.spec.name.clear(),
        |workload| workload.spec.fingerprint.clear(),
        |workload| workload.spec.fingerprint = "  ".into(),
        |workload| workload.spec.operations = 0,
        |workload| workload.samples.clear(),
        |workload| workload.samples = vec![sample(101, 20, 30, 4096)],
        |workload| workload.summary.cpu_ns.median = f64::NAN,
        |workload| workload.summary.wall_ns.max = f64::INFINITY,
    ];
    for mutate in mutations {
        let mut invalid = original.clone();
        mutate(first_workload_mut(&mut invalid)?);
        ensure!(invalid.validate().is_err());
        ensure!(compare(&original, &invalid, None).is_err());
    }
    Ok(())
}

#[test]
fn rejects_empty_samples_and_cpu_overflow() -> Result<()> {
    let spec = workload(vec![sample(1, 1, 1, 1)])?.spec;
    let empty = WorkloadReport::new(spec.clone(), vec![]);
    ensure!(empty.is_err());
    let self_overflow = WorkloadReport::new(spec.clone(), vec![sample(1, u64::MAX, 1, 1)]);
    ensure!(self_overflow.is_err());
    let mut child_user_overflow = sample(1, u64::MAX, 0, 1);
    child_user_overflow.children_user_cpu_ns = 1;
    let child_user_result = WorkloadReport::new(spec.clone(), vec![child_user_overflow]);
    ensure!(child_user_result.is_err());
    let mut child_system_overflow = sample(1, u64::MAX, 0, 1);
    child_system_overflow.children_system_cpu_ns = 1;
    let child_system_result = WorkloadReport::new(spec, vec![child_system_overflow]);
    ensure!(child_system_result.is_err());
    Ok(())
}

#[test]
fn matches_workloads_by_name() -> Result<()> {
    let mut original = report(vec![sample(100, 20, 30, 4096)])?;
    let mut second = first_workload(&original)?.clone();
    second.spec.name = "second".into();
    original.workloads.push(second);
    let mut candidate = original.clone();
    candidate.workloads.reverse();
    let comparison = compare(&original, &candidate, Some(0.0))?;
    ensure!(!comparison.regressed);
    ensure!(comparison.workloads.len() == 2);
    ensure!(comparison.added_workloads.is_empty());
    ensure!(comparison.removed_workloads.is_empty());
    ensure!(comparison.changed_workloads.is_empty());
    candidate.workloads.pop();
    let comparison = compare(&original, &candidate, None)?;
    ensure!(comparison.workloads.len() == 1);
    ensure!(comparison.removed_workloads.len() == 1);
    ensure!(comparison.removed_workloads.first() == Some(&first_workload(&original)?.spec));
    Ok(())
}

#[test]
fn adding_an_unrelated_workload_preserves_existing_comparisons() -> Result<()> {
    let original = report(vec![sample(100, 20, 30, 4096)])?;
    let mut candidate = report(vec![sample(75, 20, 30, 4096)])?;
    let mut added = workload(vec![sample(10000, 2000, 3000, 409600)])?;
    added.spec.name = "new-fixture".into();
    added.spec.fingerprint = "new-fixture-v1".into();
    let added_spec = added.spec.clone();
    candidate.workloads.push(added);
    let comparison = compare(&original, &candidate, Some(0.0))?;
    ensure!(comparison.workloads.len() == 1);
    ensure!(first_metrics(&comparison)?.wall_ns.change_percent == Some(-25.0));
    ensure!(comparison.added_workloads.len() == 1);
    ensure!(comparison.added_workloads.first() == Some(&added_spec));
    ensure!(comparison.removed_workloads.is_empty());
    ensure!(comparison.changed_workloads.is_empty());
    ensure!(!comparison.regressed);
    Ok(())
}

#[test]
fn renamed_workloads_are_added_and_removed_without_deltas() -> Result<()> {
    let original = report(vec![sample(100, 20, 30, 4096)])?;
    let mut candidate = original.clone();
    first_workload_mut(&mut candidate)?.spec.name = "renamed".into();
    let comparison = compare(&original, &candidate, Some(0.0))?;
    ensure!(comparison.workloads.is_empty());
    ensure!(comparison.changed_workloads.is_empty());
    ensure!(comparison.added_workloads.len() == 1);
    ensure!(comparison.removed_workloads.len() == 1);
    ensure!(comparison.added_workloads.first() == Some(&first_workload(&candidate)?.spec));
    ensure!(comparison.removed_workloads.first() == Some(&first_workload(&original)?.spec));
    ensure!(!comparison.regressed);
    Ok(())
}

#[test]
fn compatible_workloads_are_compared_alongside_changed_workloads() -> Result<()> {
    let mut original = report(vec![sample(100, 20, 30, 4096)])?;
    let mut changed = first_workload(&original)?.clone();
    changed.spec.name = "changed-fixture".into();
    original.workloads.push(changed.clone());
    let mut candidate = report(vec![sample(125, 20, 30, 4096)])?;
    changed.spec.fingerprint = "updated-fixture".into();
    candidate.workloads.push(changed);
    let comparison = compare(&original, &candidate, Some(10.0))?;
    ensure!(comparison.workloads.len() == 1);
    ensure!(comparison.changed_workloads.len() == 1);
    ensure!(first_metrics(&comparison)?.wall_ns.change_percent == Some(25.0));
    ensure!(comparison.regressed);
    Ok(())
}

#[test]
fn implementation_provenance_and_sample_counts_may_differ() -> Result<()> {
    let original = report(vec![sample(100, 20, 30, 4096)])?;
    let mut candidate = report(vec![
        sample(75, 20, 30, 4096),
        sample(75, 20, 30, 4096),
        sample(75, 20, 30, 4096),
    ])?;
    candidate.metadata.provenance = BTreeMap::from([
        ("revision".into(), "optimized".into()),
        ("binary_sha256".into(), "new-binary".into()),
    ]);
    let comparison = compare(&original, &candidate, None)?;
    ensure!(comparison.workloads.len() == 1);
    ensure!(first_metrics(&comparison)?.wall_ns.change_percent == Some(-25.0));
    ensure!(comparison.baseline_provenance == original.metadata.provenance);
    ensure!(comparison.candidate_provenance == candidate.metadata.provenance);
    ensure!(comparison.changed_workloads.is_empty());
    Ok(())
}

#[test]
fn report_schema_requires_per_workload_fingerprints() -> Result<()> {
    let original = report(vec![sample(100, 20, 30, 4096)])?;
    ensure!(original.schema_version == 2);
    let mut value = serde_json::to_value(&original)?;
    value
        .pointer_mut("/workloads/0/spec")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| color_eyre::eyre::eyre!("missing serialized workload"))?
        .remove("fingerprint");
    let result = serde_json::from_value::<Report>(value);
    ensure!(result.is_err());
    Ok(())
}
