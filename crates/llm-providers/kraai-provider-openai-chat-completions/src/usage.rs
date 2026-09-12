use kraai_types::{RequestCost, TokenUsage, Usd};
use serde_json::Value;

use crate::wire::ChatCompletionUsage;

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
#[expect(
    clippy::panic_in_result_fn,
    reason = "tests combine fallible fixture setup with assertions"
)]
mod tests {
    use super::*;

    #[test]
    fn reported_cost_includes_cache_writes_and_preserves_byok_cost() -> color_eyre::Result<()> {
        let raw = r#"{"prompt_tokens":150,"completion_tokens":30,"cost":0.0123,"cost_details":{"upstream_inference_cost":0.04},"prompt_tokens_details":{"cached_tokens":20,"cache_write_tokens":40},"completion_tokens_details":{"reasoning_tokens":10}}"#;
        let usage = normalize_usage(serde_json::from_str(raw)?, true)
            .ok_or_else(|| color_eyre::eyre::eyre!("missing usage"))?;
        assert_eq!(usage.input_tokens, 90);
        assert_eq!(usage.cache_write_tokens, 40);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.reasoning_tokens, 10);
        let cost = usage
            .cost
            .ok_or_else(|| color_eyre::eyre::eyre!("missing cost"))?;
        assert_eq!(cost.amount, Usd(12_300_000));
        assert_eq!(cost.upstream, Some(Usd(40_000_000)));
        assert!(cost.rates.is_none());
        Ok(())
    }

    #[test]
    fn zero_charge_without_tokens_is_preserved_and_other_endpoints_are_not_assumed_usd()
    -> color_eyre::Result<()> {
        let raw = r#"{"cost":0}"#;
        assert!(
            normalize_usage(serde_json::from_str(raw)?, true)
                .is_some_and(|usage| usage.cost.is_some_and(|cost| cost.amount == Usd(0)))
        );
        assert!(normalize_usage(serde_json::from_str(raw)?, false).is_none());
        assert!(normalize_usage(serde_json::from_str(r#"{"cost":-1}"#)?, true).is_none());
        Ok(())
    }
}
