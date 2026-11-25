use kraai_types::{TokenRates, TokenUsage, Usd};
use serde_json::Value;

const CONTEXT_OVER_200K: u64 = 200_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogCost {
    pub(super) base: Option<TokenRates>,
    pub(super) tiers: Vec<CostTier>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CostTier {
    pub(super) size: u64,
    pub(super) rates: Option<TokenRates>,
}

impl CatalogCost {
    pub(super) fn from_models_dev(cost: &Value) -> Option<Self> {
        let object = cost.as_object()?;
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "input"
                    | "output"
                    | "cache_read"
                    | "cache_write"
                    | "reasoning"
                    | "input_audio"
                    | "output_audio"
                    | "tiers"
                    | "context_over_200k"
            )
        }) {
            return None;
        }
        let tiers = match (object.get("tiers"), object.get("context_over_200k")) {
            (Some(tiers), _) => tiers
                .as_array()?
                .iter()
                .map(|entry| {
                    let tier = entry.get("tier")?.as_object()?;
                    if tier
                        .keys()
                        .any(|key| !matches!(key.as_str(), "type" | "size"))
                        || tier
                            .get("type")
                            .is_some_and(|value| value.as_str() != Some("context"))
                    {
                        return None;
                    }
                    Some(CostTier {
                        size: tier.get("size")?.as_u64()?,
                        rates: rates(entry),
                    })
                })
                .collect::<Option<_>>()?,
            (None, Some(tier)) => vec![CostTier {
                size: CONTEXT_OVER_200K,
                rates: rates(tier),
            }],
            (None, None) => Vec::new(),
        };
        Some(Self {
            base: rates(cost),
            tiers,
        })
    }
}

fn rates(value: &Value) -> Option<TokenRates> {
    let rate = |key| match value.get(key) {
        None => Ok(None),
        Some(rate) => rate
            .as_f64()
            .and_then(Usd::from_dollars)
            .map(Some)
            .ok_or(()),
    };
    let [input, output, cache_read, cache_write, reasoning] =
        ["input", "output", "cache_read", "cache_write", "reasoning"].map(rate);
    Some(TokenRates {
        input: input.ok()??,
        output: output.ok()??,
        cache_read: cache_read.ok()?,
        cache_write: cache_write.ok()?,
        reasoning: reasoning.ok()?,
    })
}

pub(crate) fn parse_rates(cost: &CatalogCost, usage: &TokenUsage) -> Option<TokenRates> {
    let prompt = usage
        .input_tokens
        .saturating_add(usage.cache_read_tokens)
        .saturating_add(usage.cache_write_tokens) as u64;
    let mut selected = &cost.base;
    let mut selected_size = 0;
    for tier in &cost.tiers {
        if prompt > tier.size && tier.size >= selected_size {
            selected = &tier.rates;
            selected_size = tier.size;
        }
    }
    selected.clone()
}
