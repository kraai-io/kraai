use serde::{Deserialize, Serialize};

use crate::{MessageId, ModelId, ProviderId, TokenUsage};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usd(pub u64);

impl Usd {
    pub fn from_dollars(value: f64) -> Option<Self> {
        let nanos = (value * 1_000_000_000.0).round();
        (value.is_finite() && value >= 0.0 && nanos < u64::MAX as f64).then_some(Self(nanos as u64))
    }
}

impl std::fmt::Display for Usd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 > 0 && self.0 < 100_000 {
            return f.write_str("<$0.0001");
        }
        let units = (u128::from(self.0) + 50_000) / 100_000;
        write!(f, "${}.{:04}", units / 10_000, units % 10_000)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRates {
    pub input: Usd,
    pub output: Usd,
    pub cache_read: Option<Usd>,
    pub cache_write: Option<Usd>,
    pub reasoning: Option<Usd>,
}

impl TokenRates {
    pub fn estimate(&self, usage: &TokenUsage) -> Option<Usd> {
        let categories = [
            (usage.input_tokens, Some(self.input)),
            (usage.output_tokens, Some(self.output)),
            (
                usage.reasoning_tokens,
                Some(self.reasoning.unwrap_or(self.output)),
            ),
            (usage.cache_read_tokens, self.cache_read),
            (usage.cache_write_tokens, self.cache_write),
        ];
        let mut total = 0_u128;
        for (tokens, rate) in categories {
            if tokens != 0 {
                total = total.checked_add((tokens as u128).checked_mul(u128::from(rate?.0))?)?;
            }
        }
        u64::try_from(total.checked_add(500_000)? / 1_000_000)
            .ok()
            .map(Usd)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestCost {
    pub amount: Usd,
    pub source: String,
    pub rates: Option<TokenRates>,
    pub priced_at: u64,
    pub upstream: Option<Usd>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestUsage {
    pub message_id: MessageId,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub started_at: u64,
    pub subscription: bool,
    #[serde(default)]
    pub unpriced_attempts: u32,
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostSummary {
    pub amount: Usd,
    pub upstream: Usd,
    pub estimated: bool,
    pub unknown: usize,
    pub subscriptions: usize,
    pub reported: usize,
    pub overflow: bool,
}

impl CostSummary {
    pub fn add(&mut self, request: &RequestUsage) {
        if request.subscription {
            self.subscriptions = self.subscriptions.saturating_add(1);
        }
        self.unknown = self
            .unknown
            .saturating_add(request.unpriced_attempts as usize);
        let Some(cost) = request.usage.as_ref().and_then(|usage| usage.cost.as_ref()) else {
            self.unknown = self.unknown.saturating_add(1);
            return;
        };
        self.reported = self.reported.saturating_add(1);
        self.estimated |= request.subscription || cost.rates.is_some();
        match self.amount.0.checked_add(cost.amount.0) {
            Some(amount) => self.amount = Usd(amount),
            None => self.overflow = true,
        }
        if let Some(upstream) = cost.upstream {
            match self.upstream.0.checked_add(upstream.0) {
                Some(amount) => self.upstream = Usd(amount),
                None => self.overflow = true,
            }
        }
    }
}

impl std::fmt::Display for CostSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.overflow {
            return f.write_str("cost overflow");
        }
        let mut parts = Vec::new();
        if self.reported != 0 || self.unknown == 0 {
            parts.push(format!(
                "{}{}",
                if self.estimated { "~" } else { "" },
                self.amount
            ));
        }
        if self.unknown != 0 {
            parts.push(String::from("$unknown"));
        }
        if self.upstream.0 != 0 {
            parts.push(format!("~{} upstream", self.upstream));
        }
        f.write_str(&parts.join(" + "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_each_token_category_once() {
        let rates = TokenRates {
            input: Usd(2_000_000_000),
            output: Usd(8_000_000_000),
            cache_read: Some(Usd(500_000_000)),
            cache_write: Some(Usd(3_000_000_000)),
            reasoning: None,
        };
        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 10,
            cache_read_tokens: 40,
            cache_write_tokens: 30,
            total_tokens: 200,
            cost: None,
        };
        assert_eq!(rates.estimate(&usage), Some(Usd(550_000)));
        let mut missing_cache_rate = rates;
        missing_cache_rate.cache_write = None;
        assert_eq!(missing_cache_rate.estimate(&usage), None);
    }

    #[test]
    fn validates_money_and_preserves_small_charges() {
        for value in [-1.0, f64::INFINITY, f64::NAN, f64::MAX] {
            assert_eq!(Usd::from_dollars(value), None);
        }
        assert_eq!(Usd::from_dollars(0.0), Some(Usd(0)));
        assert_eq!(Usd(1).to_string(), "<$0.0001");
        assert_eq!(Usd(123_450_000).to_string(), "$0.1235");
    }

    #[test]
    fn summary_includes_subscription_estimates_and_unknown_requests() {
        let mut request = RequestUsage {
            message_id: MessageId::new("request"),
            provider_id: ProviderId::new("provider"),
            model_id: ModelId::new("model"),
            started_at: 0,
            subscription: false,
            unpriced_attempts: 0,
            usage: None,
        };
        let mut summary = CostSummary::default();
        summary.add(&request);
        assert_eq!(summary.to_string(), "$unknown");
        request.usage = Some(TokenUsage {
            cost: Some(RequestCost {
                amount: Usd(0),
                source: "provider".into(),
                rates: None,
                priced_at: 0,
                upstream: None,
            }),
            ..Default::default()
        });
        summary.add(&request);
        request.subscription = true;
        summary.add(&request);
        assert_eq!(summary.to_string(), "~$0.0000 + $unknown");
        if let Some(usage) = &mut request.usage
            && let Some(cost) = &mut usage.cost
        {
            cost.amount = Usd(50_000_000);
        }
        summary.add(&request);
        assert_eq!(summary.to_string(), "~$0.0500 + $unknown");
    }

    #[test]
    fn summary_keeps_upstream_estimates_separate_and_counts_unpriced_attempts() {
        let mut request = RequestUsage {
            message_id: MessageId::new("request"),
            provider_id: ProviderId::new("openrouter"),
            model_id: ModelId::new("model"),
            started_at: 0,
            subscription: false,
            unpriced_attempts: 2,
            usage: Some(TokenUsage {
                cost: Some(RequestCost {
                    amount: Usd(0),
                    source: "openrouter".into(),
                    rates: None,
                    priced_at: 0,
                    upstream: Some(Usd(40_000_000)),
                }),
                ..Default::default()
            }),
        };
        let mut summary = CostSummary::default();
        summary.add(&request);
        assert_eq!(summary.amount, Usd(0));
        assert_eq!(
            summary.to_string(),
            "$0.0000 + $unknown + ~$0.0400 upstream"
        );
        request.usage = None;
        summary.add(&request);
        assert_eq!(summary.unknown, 5);
        request.subscription = true;
        summary.add(&request);
        assert_eq!(summary.unknown, 8);
        assert_eq!(summary.subscriptions, 1);
    }
}
