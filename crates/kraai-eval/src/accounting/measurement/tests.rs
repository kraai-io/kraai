use super::*;
use serde_json::json;

#[test]
fn decodes_request_metadata_and_context_without_coercing_fields() {
    let event = json!({
        "method": "POST", "path": "/v1/chat/completions", "timestamp_ms": 42,
        "model": " model ", "reasoning_effort": "low", "service_tier": "default",
        "reported_cost": 1200,
        "usage": {"total_tokens": 15, "input_tokens": 5, "cache_read_tokens": 2,
            "cache_write_tokens": 3, "output_tokens": 4, "reasoning_tokens": 1},
    });
    let measurement = RequestMeasurement::from_event(7, event).unwrap();
    assert!(
        serde_json::to_value(measurement).unwrap()
            == json!({
                "event_index": 7, "timestamp_ms": 42, "model": " model ",
                "reasoning_effort": "low", "service_tier": "default",
                "input_context_tokens": 10,
                "usage": {"total_tokens": 15, "input_tokens": 5, "cache_read_tokens": 2,
                    "cache_write_tokens": 3, "output_tokens": 4, "reasoning_tokens": 1},
                "reported_cost": 1200,
            }),
        "request metadata or context changed"
    );
    let event = json!({
        "method": "POST", "path": "/responses", "timestamp_ms": -1,
        "model": 17, "reasoning_effort": true, "service_tier": [], "usage": null,
        "reported_cost": "free",
    });
    let measurement = RequestMeasurement::from_event(8, event).unwrap();
    assert!(
        serde_json::to_value(measurement).unwrap()
            == json!({
                "event_index": 8, "timestamp_ms": null, "model": null,
                "reasoning_effort": null, "service_tier": null, "input_context_tokens": null,
                "usage": null, "reported_cost": null,
            }),
        "request fields were coerced"
    );
}

#[test]
fn ignores_non_model_events_before_validating_usage() {
    for event in [
        Value::Null,
        json!([]),
        json!({"method": "GET", "path": "/responses", "usage": "invalid"}),
        json!({"method": "post", "path": "/responses", "usage": "invalid"}),
        json!({"method": "POST", "path": "/models", "usage": "invalid"}),
        json!({"method": "POST", "path": "/responses?stream=true", "usage": "invalid"}),
    ] {
        assert!(
            RequestMeasurement::from_event(0, event).unwrap().is_none(),
            "non-model event was accepted"
        );
    }
}

#[test]
fn preserves_usage_errors_and_validation_order() {
    for (usage, expected) in [
        (
            json!("invalid"),
            "invalid type: string \"invalid\", expected struct UsageMetrics",
        ),
        (
            json!({"total_tokens": 1, "input_tokens": 2, "output_tokens": 0,
                "reasoning_tokens": 0, "cache_read_tokens": 0}),
            "request usage subdivisions disagree with total",
        ),
        (
            json!({"total_tokens": u64::MAX, "input_tokens": u64::MAX,
                "output_tokens": 0, "reasoning_tokens": 0, "cache_read_tokens": 1}),
            "request input tokens overflow",
        ),
    ] {
        let result = RequestMeasurement::from_event(
            0,
            json!({
                "method": "POST", "path": "/responses", "usage": usage,
            }),
        );
        assert_eq!(
            result.err().map(|error| error.to_string()).as_deref(),
            Some(expected)
        );
    }
}
