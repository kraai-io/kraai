use serde::{Deserialize, Serialize};

use crate::{MessageId, ModelId, ProviderId, TokenUsage};

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
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

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
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

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestCost {
    pub amount: Usd,
    pub source: String,
    pub rates: Option<TokenRates>,
    pub priced_at: u64,
    pub upstream: Option<Usd>,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
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

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
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
        let show_amount = self.reported != 0 || self.unknown == 0;
        if show_amount {
            write!(
                f,
                "{}{}",
                if self.estimated { "~" } else { "" },
                self.amount
            )?;
        }
        if self.unknown != 0 {
            if show_amount {
                f.write_str(" + ")?;
            }
            f.write_str("$unknown")?;
        }
        if self.upstream.0 != 0 {
            write!(f, " + ~{} upstream", self.upstream)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
