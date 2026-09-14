use serde::{Deserialize, Serialize};

use super::RequestAccounting;
use crate::PairedMetric;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairedRequestMetrics {
    pub estimated_cost_nanousd: PairedMetric,
    pub cost_basis_mismatches: u64,
    pub peak_context_tokens: PairedMetric,
    pub last_context_tokens: PairedMetric,
    pub context_paired_runs: u64,
    pub left_input_tokens: u128,
    pub right_input_tokens: u128,
    pub left_context_requests: u64,
    pub right_context_requests: u64,
    pub left_mean_context_tokens: Option<f64>,
    pub right_mean_context_tokens: Option<f64>,
}

impl PairedRequestMetrics {
    pub(crate) fn record(
        &mut self,
        left: Option<&RequestAccounting>,
        right: Option<&RequestAccounting>,
    ) {
        let (Some(left), Some(right)) = (left, right) else {
            return;
        };
        if left.complete_cost().is_some() && right.complete_cost().is_some() {
            let basis = left.pricing_basis();
            if basis.is_some() && basis == right.pricing_basis() {
                self.estimated_cost_nanousd.record(
                    left.complete_cost().map(|cost| u128::from(cost.0)),
                    right.complete_cost().map(|cost| u128::from(cost.0)),
                );
            } else {
                self.cost_basis_mismatches += 1;
            }
        }
        if let (Some(left), Some(right)) = (left.complete_context(), right.complete_context()) {
            self.peak_context_tokens.record(
                left.peak_input_tokens.map(u128::from),
                right.peak_input_tokens.map(u128::from),
            );
            self.last_context_tokens.record(
                left.last_input_tokens.map(u128::from),
                right.last_input_tokens.map(u128::from),
            );
            self.context_paired_runs += 1;
            self.left_input_tokens += left.total_input_tokens;
            self.right_input_tokens += right.total_input_tokens;
            self.left_context_requests += left.samples;
            self.right_context_requests += right.samples;
            self.left_mean_context_tokens =
                Some(self.left_input_tokens as f64 / self.left_context_requests as f64);
            self.right_mean_context_tokens =
                Some(self.right_input_tokens as f64 / self.right_context_requests as f64);
        }
    }

    pub fn display(&self) -> String {
        let number = |value: Option<f64>| {
            value.map_or_else(|| String::from("n/a"), |value| format!("{value:.1}"))
        };
        let dollars = |value: Option<f64>| {
            value.map_or_else(
                || String::from("unknown"),
                |value| format!("~${:.6}", value / 1_000_000_000.0),
            )
        };
        format!(
            "\nEstimated API cost per attempt: {} left, {} right; {} fully priced pairs; {} pricing mismatches\nMean input context per request: {} left, {} right; {} complete pairs\nMean peak input context per attempt: {} left, {} right\nMean last-recorded-request input context per attempt: {} left, {} right",
            dollars(self.estimated_cost_nanousd.left_mean),
            dollars(self.estimated_cost_nanousd.right_mean),
            self.estimated_cost_nanousd.samples,
            self.cost_basis_mismatches,
            number(self.left_mean_context_tokens),
            number(self.right_mean_context_tokens),
            self.context_paired_runs,
            number(self.peak_context_tokens.left_mean),
            number(self.peak_context_tokens.right_mean),
            number(self.last_context_tokens.left_mean),
            number(self.last_context_tokens.right_mean)
        )
    }
}
