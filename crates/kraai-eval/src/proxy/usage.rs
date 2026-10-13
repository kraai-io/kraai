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
    let usage = if usage.get("prompt_tokens").is_some() {
        kraai_provider_openai_chat_completions::usage_from_json(usage, false)
    } else {
        kraai_provider_openai_codex::usage_from_json(usage)
    }?;
    let count = |tokens: usize| u64::try_from(tokens).unwrap_or(u64::MAX);
    Some(UsageMetrics {
        total_tokens: count(usage.total_tokens),
        input_tokens: count(usage.input_tokens),
        output_tokens: count(usage.output_tokens),
        reasoning_tokens: count(usage.reasoning_tokens),
        cache_read_tokens: count(usage.cache_read_tokens),
        cache_write_tokens: count(usage.cache_write_tokens),
    })
}
