use color_eyre::eyre::{Result, ensure};
use kraai_types::Usd;
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
    #[serde(default)]
    pub reported_cost: Option<Usd>,
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
        let rejected = event
            .get("status")
            .and_then(Value::as_u64)
            .is_some_and(|status| !(200..300).contains(&status));
        if rejected && event.get("usage").is_none_or(Value::is_null) {
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
            reported_cost: event.get("reported_cost").and_then(Value::as_u64).map(Usd),
        }))
    }
}

fn take_string(event: &mut Value, key: &str) -> Option<String> {
    match event.get_mut(key)? {
        Value::String(value) => Some(std::mem::take(value)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            None
        }
    }
}

#[cfg(test)]
mod tests;
