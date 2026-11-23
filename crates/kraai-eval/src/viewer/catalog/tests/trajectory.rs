use serde_json::{Value, json};

use super::{Catalog, Fixture, JOB, harbor_result, proxy, set};

fn summary() -> Value {
    json!({"schema_version": 1, "trial_id": "trial", "input_tokens": 100,
        "cached_input_tokens": 40, "output_tokens": 20, "reasoning_tokens": 15,
        "final_context_tokens": 70, "turns": 3, "requests": 4,
        "source_url": "https://example.com/trajectory.json", "trajectory_sha256": "source"})
}

fn save(fixture: &Fixture, result: &Value, supplemental: &Value) {
    fixture.write(
        &format!("{JOB}/result.json"),
        &json!({"finished_at": "2026-09-20T10:00:02Z"}),
    );
    fixture.write(&format!("{JOB}/task__trial/result.json"), result);
    fixture.write(
        &format!("{JOB}/task__trial/agent/trajectory-metrics.json"),
        supplemental,
    );
}

#[test]
fn trajectory_supplements_public_metrics_without_double_counting_reasoning() {
    let fixture = Fixture::new();
    save(&fixture, &harbor_result(), &summary());
    let catalog = Catalog::load(&fixture.0).unwrap();
    assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
    let attempt = catalog.attempts.first().expect("missing trial");
    assert!(attempt.metrics.input_tokens == Some(100));
    assert!(attempt.metrics.output_tokens == Some(20));
    assert!(attempt.metrics.reasoning_tokens == Some(15));
    assert!(attempt.metrics.final_context_tokens == Some(70));
    assert!(attempt.metrics.turns == Some(3));
    assert!(attempt.metrics.requests == Some(4));
    let provenance = catalog
        .read_log(&attempt.id, "agent--trajectory-metrics.json")
        .unwrap();
    assert!(
        provenance
            .content
            .contains("https://example.com/trajectory.json")
    );
}

#[test]
fn invalid_trajectory_summaries_warn_without_losing_harbor_metrics() {
    for (key, value) in [
        ("schema_version", json!(2)),
        ("trial_id", json!("different-trial")),
        ("input_tokens", json!(101)),
        ("cached_input_tokens", json!(41)),
        ("output_tokens", json!(21)),
        ("reasoning_tokens", json!(21)),
        ("final_context_tokens", json!(101)),
        ("turns", json!(-1)),
        ("requests", json!(1.5)),
    ] {
        let fixture = Fixture::new();
        let mut supplemental = summary();
        set(&mut supplemental, key, value);
        save(&fixture, &harbor_result(), &supplemental);
        let catalog = Catalog::load(&fixture.0).unwrap();
        assert!(catalog.warnings.len() == 1, "{key}: {:?}", catalog.warnings);
        let attempt = catalog.attempts.first().expect("missing trial");
        assert!(attempt.metrics.input_tokens == Some(100));
        assert!(attempt.metrics.output_tokens == Some(20));
        assert!(attempt.metrics.reasoning_tokens.is_none());
        assert!(attempt.metrics.final_context_tokens.is_none());
        assert!(attempt.metrics.turns.is_none());
        assert!(attempt.metrics.requests.is_none());
    }
}

#[test]
fn missing_trajectory_measurements_remain_unknown() {
    let fixture = Fixture::new();
    let mut supplemental = summary();
    for key in [
        "reasoning_tokens",
        "final_context_tokens",
        "turns",
        "requests",
    ] {
        set(&mut supplemental, key, Value::Null);
    }
    save(&fixture, &harbor_result(), &supplemental);
    let catalog = Catalog::load(&fixture.0).unwrap();
    assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
    let attempt = catalog.attempts.first().expect("missing trial");
    assert!(attempt.metrics.input_tokens == Some(100));
    assert!(attempt.metrics.reasoning_tokens.is_none());
    assert!(attempt.metrics.final_context_tokens.is_none());
    assert!(attempt.metrics.turns.is_none());
    assert!(attempt.metrics.requests.is_none());
}

#[test]
fn adapter_and_proxy_metrics_ignore_public_trajectory_summaries() {
    for controller_proxy in [false, true] {
        let fixture = Fixture::new();
        save(&fixture, &harbor_result(), &summary());
        if controller_proxy {
            fixture.write(
                &format!("{JOB}/task__trial/kraai-controller/proxy-metrics.json"),
                &proxy(),
            );
        } else {
            fixture.write(
                &format!("{JOB}/kraai-run.json"),
                &json!({"identity": {"dataset": "benchmark", "spec": {"harness": "kraai"}}}),
            );
        }
        let catalog = Catalog::load(&fixture.0).unwrap();
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
        let attempt = catalog.attempts.first().expect("missing trial");
        assert!(attempt.metrics.turns.is_none());
        assert!(attempt.metrics.final_context_tokens.is_none());
        if controller_proxy {
            assert!(attempt.metrics.input_tokens == Some(60));
            assert!(attempt.metrics.output_tokens == Some(60));
            assert!(attempt.metrics.reasoning_tokens == Some(20));
            assert!(attempt.metrics.requests == Some(3));
        } else {
            assert!(attempt.metrics.input_tokens.is_none());
            assert!(attempt.metrics.output_tokens.is_none());
            assert!(attempt.metrics.reasoning_tokens.is_none());
            assert!(attempt.metrics.requests.is_none());
        }
    }
}
