use std::fs;

use super::*;
use color_eyre::eyre::{Result, ensure};

fn fixture() -> Result<(std::path::PathBuf, PricingOptions)> {
    let root = std::env::temp_dir().join(format!("kraai-accounting-{}", ulid::Ulid::generate()));
    fs::create_dir(&root)?;
    let config = root.join("prices.toml");
    fs::write(
        &config,
        r#"
[[provider]]
id = "test"
type = "custom"
[[model]]
id = "model"
provider_id = "test"
price_input = "2"
price_output = "8"
price_cache_read = "0.5"
price_cache_write = "3"
"#,
    )?;
    Ok((
        root,
        PricingOptions {
            config: Some(config),
            provider: None,
            ..Default::default()
        },
    ))
}

fn event(input: u64, cached: u64, output: u64, reasoning: u64) -> serde_json::Value {
    serde_json::json!({"method":"POST", "path":"/v1/responses", "timestamp_ms":1,"model":"model", "reasoning_effort":"low", "usage":{
        "total_tokens":input+cached+output+reasoning,"input_tokens":input,"cache_read_tokens":cached,"output_tokens":output,"reasoning_tokens":reasoning
    }})
}

#[test]
fn prices_cached_and_reasoning_tokens_once_and_measures_each_request_context() -> Result<()> {
    let (root, options) = fixture()?;
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
    let result = analyze_requests(&path, 3, &options)?;
    ensure!(result.model_requests == 2 && result.context.samples == 2);
    ensure!(result.context.mean_input_tokens == Some(500.0));
    ensure!(result.context.peak_input_tokens == Some(500));
    ensure!(result.context.last_input_tokens == Some(500));
    ensure!(result.complete_cost() == Some(Usd(1_420_000)));
    ensure!(result.requests.iter().all(|request| {
        request
            .estimated_cost
            .as_ref()
            .is_some_and(|cost| cost.source == "configured" && cost.rates.is_some())
    }));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn missing_usage_or_unrecorded_requests_never_look_like_complete_cheap_runs() -> Result<()> {
    let (root, options) = fixture()?;
    let path = root.join("proxy.events.jsonl");
    let mut missing = event(1, 0, 1, 0);
    missing
        .as_object_mut()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing object"))?
        .remove("usage");
    fs::write(&path, format!("{}\n{missing}\n", event(100, 0, 10, 0)))?;
    let result = analyze_requests(&path, 3, &options)?;
    ensure!(result.unrecorded_requests == 1 && result.unpriced_requests == 1);
    ensure!(result.complete_cost().is_none() && result.complete_context().is_none());
    ensure!(result.context.last_input_tokens.is_none());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn non_default_service_tiers_stay_unpriced_and_stale_sidecars_are_rejected() -> Result<()> {
    let (root, options) = fixture()?;
    let path = root.join("proxy.events.jsonl");
    let mut request = event(100, 0, 10, 0);
    request
        .as_object_mut()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing object"))?
        .insert(String::from("service_tier"), serde_json::json!("priority"));
    fs::write(&path, format!("{request}\n"))?;
    let result = analyze_requests(&path, 1, &options)?;
    ensure!(result.complete_context().is_some() && result.complete_cost().is_none());
    let sidecar = root.join("request-accounting.json");
    fs::write(&sidecar, serde_json::to_vec(&result)?)?;
    ensure!(load_accounting(&sidecar)?.is_some());
    fs::write(&path, "")?;
    ensure!(load_accounting(&sidecar).is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn comparisons_exclude_mismatched_price_tables_but_keep_context_measurements() -> Result<()> {
    let (root, options) = fixture()?;
    let path = root.join("proxy.events.jsonl");
    fs::write(&path, format!("{}\n", event(100, 400, 20, 10)))?;
    let left = analyze_requests(&path, 1, &options)?;
    let config = root.join("prices.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)?.replace("price_input = \"2\"", "price_input = \"20\""),
    )?;
    let right = analyze_requests(&path, 1, &options)?;
    let mut comparison = PairedRequestMetrics::default();
    comparison.record(Some(&left), Some(&right));
    ensure!(
        comparison.estimated_cost_nanousd.samples == 0 && comparison.cost_basis_mismatches == 1
    );
    ensure!(
        comparison.context_paired_runs == 1 && comparison.left_mean_context_tokens == Some(500.0)
    );
    let mut equal = PairedRequestMetrics::default();
    equal.record(Some(&left), Some(&left));
    ensure!(equal.estimated_cost_nanousd.samples == 1 && equal.cost_basis_mismatches == 0);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn incomplete_proxy_coverage_cannot_fall_back_to_harness_totals() -> Result<()> {
    let (root, options) = fixture()?;
    let path = root.join("proxy.events.jsonl");
    fs::write(&path, format!("{}\n", event(100, 400, 20, 10)))?;
    let accounting = analyze_requests(&path, 2, &options)?;
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

#[test]
fn frozen_pricing_survives_source_configuration_removal() -> Result<()> {
    let (root, options) = fixture()?;
    let frozen = options.freeze()?;
    fs::remove_file(root.join("prices.toml"))?;
    let path = root.join("proxy.events.jsonl");
    fs::write(&path, format!("{}\n", event(100, 400, 20, 10)))?;
    let result = analyze_requests(&path, 1, &frozen)?;
    ensure!(result.complete_cost() == Some(Usd(640_000)));
    fs::remove_dir_all(root)?;
    Ok(())
}
