use super::*;

#[test]
fn reported_cost_includes_cache_writes_and_preserves_byok_cost() {
    let raw = r#"{"prompt_tokens":150,"completion_tokens":30,"cost":0.0123,"cost_details":{"upstream_inference_cost":0.04},"prompt_tokens_details":{"cached_tokens":20,"cache_write_tokens":40},"completion_tokens_details":{"reasoning_tokens":10}}"#;
    let usage = normalize_usage(serde_json::from_str(raw).unwrap(), true).expect("missing usage");
    assert_eq!(usage.input_tokens, 90);
    assert_eq!(usage.cache_write_tokens, 40);
    assert_eq!(usage.output_tokens, 20);
    assert_eq!(usage.reasoning_tokens, 10);
    let cost = usage.cost.expect("missing cost");
    assert_eq!(cost.amount, Usd(12_300_000));
    assert_eq!(cost.upstream, Some(Usd(40_000_000)));
    assert!(cost.rates.is_none());
}

#[test]
fn zero_charge_without_tokens_is_preserved_and_other_endpoints_are_not_assumed_usd() {
    let raw = r#"{"cost":0}"#;
    assert!(
        normalize_usage(serde_json::from_str(raw).unwrap(), true)
            .is_some_and(|usage| usage.cost.is_some_and(|cost| cost.amount == Usd(0)))
    );
    assert!(normalize_usage(serde_json::from_str(raw).unwrap(), false).is_none());
    assert!(normalize_usage(serde_json::from_str(r#"{"cost":-1}"#).unwrap(), true).is_none());
}
