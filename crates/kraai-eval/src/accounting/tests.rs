use std::fs;

use super::*;
use crate::UsageMetrics;
use color_eyre::eyre::{Result, ensure};

fn fixture() -> Result<std::path::PathBuf> {
    let root = std::env::temp_dir().join(format!("kraai-accounting-{}", ulid::Ulid::generate()));
    fs::create_dir(&root)?;
    Ok(root)
}

fn event(input: u64, cached: u64, output: u64, reasoning: u64) -> serde_json::Value {
    serde_json::json!({"method":"POST", "path":"/v1/responses", "timestamp_ms":1,"model":"model", "reasoning_effort":"low", "usage":{
        "total_tokens":input+cached+output+reasoning,"input_tokens":input,"cache_read_tokens":cached,"output_tokens":output,"reasoning_tokens":reasoning
    }})
}

#[test]
fn measures_each_request_context_and_keeps_its_usage() -> Result<()> {
    let root = fixture()?;
    let path = root.join("proxy.events.jsonl");
    fs::write(
        &path,
        format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"method":"GET","path":"/models"}),
            event(100, 400, 20, 10),
            event(300, 200, 10, 0)
        ),
    )?;
    let result = analyze_requests(&path, 3)?;
    ensure!(result.model_requests == 2 && result.context.samples == 2);
    ensure!(result.context.mean_input_tokens == Some(500.0));
    ensure!(result.context.peak_input_tokens == Some(500));
    ensure!(result.context.last_input_tokens == Some(500));
    ensure!(
        result.requests.iter().all(|request| {
            request.model.as_deref() == Some("model") && request.usage.is_some()
        })
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn missing_usage_or_unrecorded_requests_never_look_like_complete_cheap_runs() -> Result<()> {
    let root = fixture()?;
    let path = root.join("proxy.events.jsonl");
    let mut missing = event(1, 0, 1, 0);
    missing
        .as_object_mut()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing object"))?
        .remove("usage");
    fs::write(&path, format!("{}\n{missing}\n", event(100, 0, 10, 0)))?;
    let result = analyze_requests(&path, 3)?;
    ensure!(result.unrecorded_requests == 1 && result.model_requests == 2);
    ensure!(
        result
            .requests
            .get(1)
            .is_some_and(|request| request.usage.is_none())
    );
    ensure!(result.complete_context().is_none());
    ensure!(result.context.last_input_tokens.is_none());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn rejected_requests_without_usage_are_not_model_requests() -> Result<()> {
    let root = fixture()?;
    let path = root.join("proxy.events.jsonl");
    let mut rejected = event(1, 0, 1, 0);
    let object = rejected
        .as_object_mut()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing object"))?;
    object.remove("usage");
    object.insert("status".into(), serde_json::json!(503));
    let mut answered = event(100, 0, 10, 0);
    answered
        .as_object_mut()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing object"))?
        .insert("status".into(), serde_json::json!(200));
    fs::write(&path, format!("{rejected}\n{answered}\n"))?;
    let result = analyze_requests(&path, 2)?;
    ensure!(result.model_requests == 1 && result.complete_context().is_some());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn service_tiers_are_recorded_and_stale_sidecars_are_rejected() -> Result<()> {
    let root = fixture()?;
    let path = root.join("proxy.events.jsonl");
    let standard = event(100, 400, 20, 10);
    fs::write(&path, format!("{standard}\n"))?;
    for tier in ["default", "auto", "priority", "fast", "ultrafast", "flex"] {
        let mut request = standard.clone();
        request
            .as_object_mut()
            .ok_or_else(|| color_eyre::eyre::eyre!("missing object"))?
            .insert("service_tier".into(), serde_json::json!(tier));
        fs::write(&path, format!("{request}\n"))?;
        let result = analyze_requests(&path, 1)?;
        ensure!(result.complete_context().is_some());
        ensure!(
            result
                .requests
                .first()
                .and_then(|request| request.service_tier.as_deref())
                == Some(tier)
        );
    }
    let result = analyze_requests(&path, 1)?;
    let sidecar = root.join("request-accounting.json");
    fs::write(&sidecar, serde_json::to_vec(&result)?)?;
    ensure!(load_accounting(&sidecar)?.is_some());
    fs::write(&path, "")?;
    ensure!(load_accounting(&sidecar).is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn incomplete_proxy_coverage_cannot_fall_back_to_harness_totals() -> Result<()> {
    let root = fixture()?;
    let path = root.join("proxy.events.jsonl");
    fs::write(&path, format!("{}\n", event(100, 400, 20, 10)))?;
    let accounting = analyze_requests(&path, 2)?;
    let usage = UsageMetrics {
        input_tokens: 100,
        cache_read_tokens: 400,
        output_tokens: 20,
        reasoning_tokens: 10,
        total_tokens: 530,
        ..Default::default()
    };
    let harness = serde_json::from_value(serde_json::json!({"schema_version":1,"usage":usage}))?;
    let metrics = crate::EvaluationMetrics {
        proxy: Some(crate::ProxyMetrics {
            usage,
            accounting: Some(accounting),
            ..Default::default()
        }),
        harness: Some(harness),
    };
    ensure!(metrics.usage().is_none());
    fs::remove_dir_all(root)?;
    Ok(())
}
