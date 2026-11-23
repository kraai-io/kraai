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

fn workload(samples: Vec<Sample>) -> WorkloadReport {
    WorkloadReport::new(
        WorkloadSpec {
            name: "fixture".into(),
            fingerprint: "fixture-v1".into(),
            operations: 10,
            parameters: serde_json::json!({"bytes": 1024}),
        },
        samples,
    )
    .unwrap()
}

fn report(samples: Vec<Sample>) -> Report {
    Report::new(
        Metadata {
            measurement_schema: "process-v1".into(),
            host: BTreeMap::from([("cpu".into(), "test".into())]),
            build: BTreeMap::from([("profile".into(), "release".into())]),
            provenance: BTreeMap::from([("revision".into(), "baseline".into())]),
            warmups: 1,
        },
        vec![workload(samples)],
    )
    .unwrap()
}

fn first_workload(report: &Report) -> &WorkloadReport {
    report.workloads.first().expect("missing test workload")
}

fn first_workload_mut(report: &mut Report) -> &mut WorkloadReport {
    report.workloads.first_mut().expect("missing test workload")
}

fn first_metrics(comparison: &Comparison) -> &Metrics<MetricComparison> {
    comparison
        .workloads
        .first()
        .map(|workload| &workload.metrics)
        .expect("missing test comparison")
}

#[test]
fn computes_medians_and_median_absolute_deviation() {
    let result = workload(vec![
        sample(100, 8, 1, 2048),
        sample(10, 1, 8, 4096),
        sample(12, 3, 3, 1024),
        sample(14, 2, 2, 8192),
    ]);
    assert!(
        result.summary.wall_ns
            == Statistics {
                min: 10.0,
                max: 100.0,
                median: 13.0,
                mad: 2.0,
            }
    );
    assert!(result.summary.cpu_ns.median == 7.5);
    assert!(result.summary.peak_rss_bytes.median == 3072.0);
    let odd = workload(vec![
        sample(20, 0, 0, 0),
        sample(1, 0, 0, 0),
        sample(4, 0, 0, 0),
    ]);
    assert!(odd.summary.wall_ns.median == 4.0);
    assert!(odd.summary.wall_ns.mad == 3.0);
}

#[test]
fn total_cpu_includes_children_and_memory_peaks_stay_separate() {
    let mut measured = sample(100, 10, 20, 4096);
    measured.children_user_cpu_ns = 30;
    measured.children_system_cpu_ns = 40;
    measured.children_peak_rss_bytes = 8192;
    let result = workload(vec![measured]);
    assert!(result.summary.cpu_ns.median == 100.0);
    assert!(result.summary.user_cpu_ns.median == 10.0);
    assert!(result.summary.system_cpu_ns.median == 20.0);
    assert!(result.summary.children_user_cpu_ns.median == 30.0);
    assert!(result.summary.children_system_cpu_ns.median == 40.0);
    assert!(result.summary.peak_rss_bytes.median == 4096.0);
    assert!(result.summary.children_peak_rss_bytes.median == 8192.0);
}

#[test]
fn round_trip_preserves_report_validation() {
    let original = report(vec![
        sample(1_234_567_891, 20, 30, 4096),
        sample(1_234_567_892, 21, 33, 4097),
        sample(1_234_567_895, 23, 34, 4099),
        sample(1_234_567_897, 24, 36, 4103),
    ]);
    assert!(first_workload(&original).summary.wall_ns.median.fract() != 0.0);
    let json = serde_json::to_string(&original).unwrap();
    let restored: Report = serde_json::from_str(&json).unwrap();
    restored.validate().unwrap();
    assert!(first_workload(&restored).summary == first_workload(&original).summary);
}

#[test]
fn compares_medians_without_gating_cpu_components() {
    let before = report(vec![sample(100, 10, 90, 1000)]);
    let mut updated = sample(110, 90, 0, 900);
    updated.children_system_cpu_ns = 10;
    let mut after = report(vec![updated]);
    after
        .metadata
        .provenance
        .insert("revision".into(), "candidate".into());
    let result = compare(&before, &after, Some(10.0)).unwrap();
    let metrics = first_metrics(&result);
    assert!(metrics.wall_ns.change_percent == Some(10.0));
    assert!(metrics.peak_rss_bytes.change_percent == Some(-10.0));
    assert!(metrics.cpu_ns.change_percent == Some(0.0));
    assert!(metrics.user_cpu_ns.change_percent == Some(800.0));
    assert!(!metrics.user_cpu_ns.regressed);
    assert!(!metrics.children_system_cpu_ns.regressed);
    assert!(!result.regressed);
    assert!(compare(&before, &after, Some(9.0)).unwrap().regressed);
    assert!(!compare(&before, &after, None).unwrap().regressed);
}

#[test]
fn gates_total_cpu_and_both_memory_peaks() {
    let mut baseline = sample(100, 10, 10, 1000);
    baseline.children_user_cpu_ns = 10;
    baseline.children_system_cpu_ns = 10;
    baseline.children_peak_rss_bytes = 1000;
    let before = report(vec![baseline.clone()]);
    let mutations: [fn(&mut Sample); 3] = [
        |sample| sample.children_user_cpu_ns += 10,
        |sample| sample.peak_rss_bytes += 200,
        |sample| sample.children_peak_rss_bytes += 200,
    ];
    for mutate in mutations {
        let mut updated = baseline.clone();
        mutate(&mut updated);
        let after = report(vec![updated]);
        assert!(compare(&before, &after, Some(10.0)).unwrap().regressed);
    }
}

#[test]
fn zero_baselines_never_produce_infinite_percentages() {
    let before = report(vec![sample(0, 0, 0, 0)]);
    let after = report(vec![sample(1, 0, 1, 0)]);
    let result = compare(&before, &after, Some(100.0)).unwrap();
    assert!(result.regressed);
    let metrics = first_metrics(&result);
    assert!(metrics.wall_ns.change_percent.is_none());
    assert!(metrics.peak_rss_bytes.change_percent == Some(0.0));
    let json = serde_json::to_value(&result).unwrap();
    assert!(
        json.pointer("/workloads/0/metrics/wall_ns/change_percent")
            == Some(&serde_json::Value::Null)
    );
}

#[test]
fn rejects_invalid_regression_budgets() {
    let original = report(vec![sample(100, 20, 30, 4096)]);
    for threshold in [-1.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        assert!(compare(&original, &original, Some(threshold)).is_err());
    }
}

#[test]
fn rejects_incompatible_metadata() {
    let original = report(vec![sample(100, 20, 30, 4096)]);
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
        candidate.validate().unwrap();
        assert!(compare(&original, &candidate, None).is_err());
    }
}

#[test]
fn classifies_changed_fixtures_without_emitting_metric_deltas() {
    let original = report(vec![sample(100, 20, 30, 4096)]);
    let mutations: [fn(&mut WorkloadSpec); 4] = [
        |spec| spec.fingerprint = "fixture-v2".into(),
        |spec| spec.operations += 1,
        |spec| spec.parameters = serde_json::json!({"bytes": 2048}),
        |spec| spec.parameters = serde_json::json!({"bytes": 1024, "description": "changed"}),
    ];
    for mutate in mutations {
        let mut candidate = report(vec![sample(1000, 200, 300, 40960)]);
        mutate(&mut first_workload_mut(&mut candidate).spec);
        candidate.validate().unwrap();
        let comparison = compare(&original, &candidate, Some(0.0)).unwrap();
        assert!(comparison.workloads.is_empty());
        assert!(comparison.added_workloads.is_empty());
        assert!(comparison.removed_workloads.is_empty());
        assert!(comparison.changed_workloads.len() == 1);
        assert!(!comparison.regressed);
        let changed = comparison
            .changed_workloads
            .first()
            .expect("missing changed fixture");
        assert!(changed.baseline == first_workload(&original).spec);
        assert!(changed.candidate == first_workload(&candidate).spec);
        let serialized = serde_json::to_value(&comparison).unwrap();
        assert!(serialized.pointer("/changed_workloads/0/metrics").is_none());
    }
}

#[test]
fn rejects_malformed_reports() {
    let original = report(vec![sample(100, 20, 30, 4096)]);
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
        assert!(invalid.validate().is_err());
        assert!(compare(&original, &invalid, None).is_err());
    }
    let mut duplicate = original.clone();
    duplicate.workloads.push(first_workload(&original).clone());
    assert!(duplicate.validate().is_err());
}

#[test]
fn rejects_malformed_workloads() {
    let original = report(vec![sample(100, 20, 30, 4096)]);
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
        mutate(first_workload_mut(&mut invalid));
        assert!(invalid.validate().is_err());
        assert!(compare(&original, &invalid, None).is_err());
    }
}

#[test]
fn rejects_empty_samples_and_cpu_overflow() {
    let spec = workload(vec![sample(1, 1, 1, 1)]).spec;
    let empty = WorkloadReport::new(spec.clone(), vec![]);
    assert!(empty.is_err());
    let self_overflow = WorkloadReport::new(spec.clone(), vec![sample(1, u64::MAX, 1, 1)]);
    assert!(self_overflow.is_err());
    let mut child_user_overflow = sample(1, u64::MAX, 0, 1);
    child_user_overflow.children_user_cpu_ns = 1;
    let child_user_result = WorkloadReport::new(spec.clone(), vec![child_user_overflow]);
    assert!(child_user_result.is_err());
    let mut child_system_overflow = sample(1, u64::MAX, 0, 1);
    child_system_overflow.children_system_cpu_ns = 1;
    let child_system_result = WorkloadReport::new(spec, vec![child_system_overflow]);
    assert!(child_system_result.is_err());
}

#[test]
fn matches_workloads_by_name() {
    let mut original = report(vec![sample(100, 20, 30, 4096)]);
    let mut second = first_workload(&original).clone();
    second.spec.name = "second".into();
    original.workloads.push(second);
    let mut candidate = original.clone();
    candidate.workloads.reverse();
    let comparison = compare(&original, &candidate, Some(0.0)).unwrap();
    assert!(!comparison.regressed);
    assert!(comparison.workloads.len() == 2);
    assert!(comparison.added_workloads.is_empty());
    assert!(comparison.removed_workloads.is_empty());
    assert!(comparison.changed_workloads.is_empty());
    candidate.workloads.pop();
    let comparison = compare(&original, &candidate, None).unwrap();
    assert!(comparison.workloads.len() == 1);
    assert!(comparison.removed_workloads.len() == 1);
    assert!(comparison.removed_workloads.first() == Some(&first_workload(&original).spec));
}

#[test]
fn adding_an_unrelated_workload_preserves_existing_comparisons() {
    let original = report(vec![sample(100, 20, 30, 4096)]);
    let mut candidate = report(vec![sample(75, 20, 30, 4096)]);
    let mut added = workload(vec![sample(10000, 2000, 3000, 409600)]);
    added.spec.name = "new-fixture".into();
    added.spec.fingerprint = "new-fixture-v1".into();
    let added_spec = added.spec.clone();
    candidate.workloads.push(added);
    let comparison = compare(&original, &candidate, Some(0.0)).unwrap();
    assert!(comparison.workloads.len() == 1);
    assert!(first_metrics(&comparison).wall_ns.change_percent == Some(-25.0));
    assert!(comparison.added_workloads.len() == 1);
    assert!(comparison.added_workloads.first() == Some(&added_spec));
    assert!(comparison.removed_workloads.is_empty());
    assert!(comparison.changed_workloads.is_empty());
    assert!(!comparison.regressed);
}

#[test]
fn renamed_workloads_are_added_and_removed_without_deltas() {
    let original = report(vec![sample(100, 20, 30, 4096)]);
    let mut candidate = original.clone();
    first_workload_mut(&mut candidate).spec.name = "renamed".into();
    let comparison = compare(&original, &candidate, Some(0.0)).unwrap();
    assert!(comparison.workloads.is_empty());
    assert!(comparison.changed_workloads.is_empty());
    assert!(comparison.added_workloads.len() == 1);
    assert!(comparison.removed_workloads.len() == 1);
    assert!(comparison.added_workloads.first() == Some(&first_workload(&candidate).spec));
    assert!(comparison.removed_workloads.first() == Some(&first_workload(&original).spec));
    assert!(!comparison.regressed);
}

#[test]
fn compatible_workloads_are_compared_alongside_changed_workloads() {
    let mut original = report(vec![sample(100, 20, 30, 4096)]);
    let mut changed = first_workload(&original).clone();
    changed.spec.name = "changed-fixture".into();
    original.workloads.push(changed.clone());
    let mut candidate = report(vec![sample(125, 20, 30, 4096)]);
    changed.spec.fingerprint = "updated-fixture".into();
    candidate.workloads.push(changed);
    let comparison = compare(&original, &candidate, Some(10.0)).unwrap();
    assert!(comparison.workloads.len() == 1);
    assert!(comparison.changed_workloads.len() == 1);
    assert!(first_metrics(&comparison).wall_ns.change_percent == Some(25.0));
    assert!(comparison.regressed);
}

#[test]
fn implementation_provenance_and_sample_counts_may_differ() {
    let original = report(vec![sample(100, 20, 30, 4096)]);
    let mut candidate = report(vec![
        sample(75, 20, 30, 4096),
        sample(75, 20, 30, 4096),
        sample(75, 20, 30, 4096),
    ]);
    candidate.metadata.provenance = BTreeMap::from([
        ("revision".into(), "optimized".into()),
        ("binary_sha256".into(), "new-binary".into()),
    ]);
    let comparison = compare(&original, &candidate, None).unwrap();
    assert!(comparison.workloads.len() == 1);
    assert!(first_metrics(&comparison).wall_ns.change_percent == Some(-25.0));
    assert!(comparison.baseline_provenance == original.metadata.provenance);
    assert!(comparison.candidate_provenance == candidate.metadata.provenance);
    assert!(comparison.changed_workloads.is_empty());
}
