use color_eyre::eyre::{Result, ensure};
use kraai_types::RequestCost;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::UsageMetrics;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMeasurement {
    pub event_index: usize,
    pub timestamp_ms: Option<u128>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub input_context_tokens: Option<u64>,
    pub usage: Option<UsageMetrics>,
    pub estimated_cost: Option<RequestCost>,
    #[serde(default)]
    pub pricing_basis: Option<String>,
    pub unpriced_reason: Option<String>,
}

impl RequestMeasurement {
    pub(super) fn from_event(event_index: usize, mut event: Value) -> Result<Option<Self>> {
        if event.get("method").and_then(Value::as_str) != Some("POST") {
            return Ok(None);
        }
        let path = event
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !path.ends_with("/responses") && !path.ends_with("/chat/completions") {
            return Ok(None);
        }
        let usage = event
            .get_mut("usage")
            .filter(|value| !value.is_null())
            .map(std::mem::take)
            .map(serde_json::from_value::<UsageMetrics>)
            .transpose()?;
        let input = usage
            .as_ref()
            .map(|usage| {
                ensure!(
                    usage.total_tokens == usage.used_context_tokens(),
                    "request usage subdivisions disagree with total"
                );
                usage
                    .input_tokens
                    .checked_add(usage.cache_read_tokens)
                    .and_then(|input| input.checked_add(usage.cache_write_tokens))
                    .ok_or_else(|| color_eyre::eyre::eyre!("request input tokens overflow"))
            })
            .transpose()?;
        Ok(Some(Self {
            event_index,
            timestamp_ms: event
                .get("timestamp_ms")
                .and_then(Value::as_u64)
                .map(u128::from),
            model: take_string(&mut event, "model"),
            reasoning_effort: take_string(&mut event, "reasoning_effort"),
            service_tier: take_string(&mut event, "service_tier"),
            input_context_tokens: input,
            usage,
            estimated_cost: None,
            pricing_basis: None,
            unpriced_reason: None,
        }))
    }
}

fn take_string(event: &mut Value, key: &str) -> Option<String> {
    match event.get_mut(key)? {
        Value::String(value) => Some(std::mem::take(value)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_request_metadata_and_context_without_coercing_fields() -> Result<()> {
        let event = json!({
            "method": "POST", "path": "/v1/chat/completions", "timestamp_ms": 42,
            "model": " model ", "reasoning_effort": "low", "service_tier": "default",
            "usage": {"total_tokens": 15, "input_tokens": 5, "cache_read_tokens": 2,
                "cache_write_tokens": 3, "output_tokens": 4, "reasoning_tokens": 1},
        });
        let measurement = RequestMeasurement::from_event(7, event)?;
        ensure!(
            serde_json::to_value(measurement)?
                == json!({
                    "event_index": 7, "timestamp_ms": 42, "model": " model ",
                    "reasoning_effort": "low", "service_tier": "default",
                    "input_context_tokens": 10,
                    "usage": {"total_tokens": 15, "input_tokens": 5, "cache_read_tokens": 2,
                        "cache_write_tokens": 3, "output_tokens": 4, "reasoning_tokens": 1},
                    "estimated_cost": null, "pricing_basis": null, "unpriced_reason": null,
                }),
            "request metadata or context changed"
        );
        let event = json!({
            "method": "POST", "path": "/responses", "timestamp_ms": -1,
            "model": 17, "reasoning_effort": true, "service_tier": [], "usage": null,
        });
        let measurement = RequestMeasurement::from_event(8, event)?;
        ensure!(
            serde_json::to_value(measurement)?
                == json!({
                    "event_index": 8, "timestamp_ms": null, "model": null,
                    "reasoning_effort": null, "service_tier": null, "input_context_tokens": null,
                    "usage": null, "estimated_cost": null, "pricing_basis": null,
                    "unpriced_reason": null,
                }),
            "request fields were coerced"
        );
        Ok(())
    }

    #[test]
    fn ignores_non_model_events_before_validating_usage() -> Result<()> {
        for event in [
            Value::Null,
            json!([]),
            json!({"method": "GET", "path": "/responses", "usage": "invalid"}),
            json!({"method": "post", "path": "/responses", "usage": "invalid"}),
            json!({"method": "POST", "path": "/models", "usage": "invalid"}),
            json!({"method": "POST", "path": "/responses?stream=true", "usage": "invalid"}),
        ] {
            ensure!(
                RequestMeasurement::from_event(0, event)?.is_none(),
                "non-model event was accepted"
            );
        }
        Ok(())
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
}
