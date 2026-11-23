use kraai_types::{RequestCost, TokenUsage, Usd};
use serde_json::Value;

use crate::wire::ChatCompletionUsage;

pub fn usage_from_json(usage: &Value, reported_costs: bool) -> Option<TokenUsage> {
    serde_json::from_value::<ChatCompletionUsage>(usage.clone())
        .ok()
        .and_then(|usage| normalize_usage(usage, reported_costs))
}

pub(super) fn normalize_usage(
    usage: ChatCompletionUsage,
    reported_costs: bool,
) -> Option<TokenUsage> {
    let cache_read_tokens = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|details| details.cached_tokens)
        .unwrap_or_default();
    let cache_write_tokens = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|details| details.cache_write_tokens)
        .unwrap_or_default();
    let reasoning_tokens = usage
        .completion_tokens_details
        .and_then(|details| details.reasoning_tokens)
        .unwrap_or_default();
    let input_tokens = usage
        .prompt_tokens
        .saturating_sub(cache_read_tokens)
        .saturating_sub(cache_write_tokens);
    let output_tokens = usage.completion_tokens.saturating_sub(reasoning_tokens);
    let cost = reported_costs
        .then(|| usage.cost.as_ref().and_then(dollars))
        .flatten()
        .map(|amount| RequestCost {
            amount,
            source: String::from("openrouter"),
            rates: None,
            priced_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            upstream: usage
                .cost_details
                .as_ref()
                .and_then(|details| details.get("upstream_inference_cost"))
                .and_then(dollars),
        });
    let total_tokens = usage
        .total_tokens
        .unwrap_or_else(|| usage.prompt_tokens.saturating_add(usage.completion_tokens));
    if total_tokens == 0
        && usage.prompt_tokens == 0
        && usage.completion_tokens == 0
        && cost.is_none()
    {
        return None;
    }
    Some(TokenUsage {
        total_tokens,
        input_tokens,
        output_tokens,
        reasoning_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cost,
    })
}

fn dollars(value: &Value) -> Option<Usd> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .and_then(Usd::from_dollars)
}

#[cfg(test)]
mod tests;
