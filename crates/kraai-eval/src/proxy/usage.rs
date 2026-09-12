use crate::metrics::UsageMetrics;

pub(super) fn usage_from_response_body(body: &[u8]) -> Option<UsageMetrics> {
    let text = std::str::from_utf8(body).ok()?;
    let mut usage = None;
    for line in text.lines() {
        let payload = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
            continue;
        };
        if let Some(candidate) = usage_from_json(&value) {
            usage = Some(candidate);
        }
    }
    usage
}

fn usage_from_json(value: &serde_json::Value) -> Option<UsageMetrics> {
    let usage = value
        .get("response")
        .and_then(|response| response.get("usage"))
        .or_else(|| value.get("usage"))?;
    let cached = nested_u64(usage, "input_tokens_details", "cached_tokens")
        .or_else(|| nested_u64(usage, "prompt_tokens_details", "cached_tokens"))
        .unwrap_or_default();
    let reasoning = nested_u64(usage, "output_tokens_details", "reasoning_tokens")
        .or_else(|| nested_u64(usage, "completion_tokens_details", "reasoning_tokens"))
        .unwrap_or_default();
    let raw_input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let raw_output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let total = usage
        .get("total_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| raw_input.saturating_add(raw_output));
    (total != 0 || raw_input != 0 || raw_output != 0).then_some(UsageMetrics {
        total_tokens: total,
        input_tokens: raw_input.saturating_sub(cached),
        output_tokens: raw_output.saturating_sub(reasoning),
        reasoning_tokens: reasoning,
        cache_read_tokens: cached,
    })
}

fn nested_u64(value: &serde_json::Value, object: &str, field: &str) -> Option<u64> {
    value
        .get(object)
        .and_then(|details| details.get(field))
        .and_then(serde_json::Value::as_u64)
}
