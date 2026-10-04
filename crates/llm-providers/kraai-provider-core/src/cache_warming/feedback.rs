use std::collections::VecDeque;

use kraai_types::{TokenRates, TokenUsage};

use super::{CacheWarmingPolicy, CompletedWarmup, prefix::Prefix};

const SAMPLE_COUNT: usize = 5;
const CACHE_REUSE_PROBABILITY: f64 = 0.85;

#[derive(Default)]
pub(super) struct Feedback {
    rates: Option<TokenRates>,
    growth: VecDeque<f64>,
    output: VecDeque<(f64, f64)>,
    latest: Option<Measurement>,
}

struct Measurement {
    prefix: Prefix,
    suffix: Vec<u64>,
    input: usize,
    prefix_tokens: Option<f64>,
    cached: usize,
}

pub(super) struct Payback {
    pub(super) warmup_cost: f64,
    pub(super) expected_saving: f64,
}

pub(super) fn input_tokens(usage: &TokenUsage) -> usize {
    usage
        .input_tokens
        .saturating_add(usage.cache_read_tokens)
        .saturating_add(usage.cache_write_tokens)
}

impl Feedback {
    pub(super) fn synchronize(&mut self, prefix: &Prefix) {
        if self
            .latest
            .as_ref()
            .is_some_and(|last| !prefix.extends(&last.prefix))
        {
            self.latest = None;
            self.growth.clear();
        }
    }

    pub(super) fn observe(
        &mut self,
        prefix: Prefix,
        suffix: Vec<u64>,
        usage: &TokenUsage,
        completed: Option<&CompletedWarmup>,
    ) {
        self.observe_rates(usage);
        self.synchronize(&prefix);
        let input = input_tokens(usage);
        let mut prefix_tokens = self.predicted_prefix(&prefix);
        if let Some(last) = &self.latest
            && last.prefix != prefix
            && last.suffix == suffix
        {
            if let Some(growth) = input.checked_sub(last.input) {
                push_sample(&mut self.growth, growth as f64);
                prefix_tokens = last.prefix_tokens.map(|tokens| tokens + growth as f64);
            } else {
                self.growth.clear();
                prefix_tokens = None;
            }
        }
        if let Some(warmup) = completed.filter(|warmup| warmup.prefix == prefix) {
            prefix_tokens = (warmup.input_tokens <= input).then_some(warmup.input_tokens as f64);
        }
        self.latest = Some(Measurement {
            prefix,
            suffix,
            input,
            prefix_tokens: prefix_tokens.map(|tokens| tokens.min(input as f64)),
            cached: usage.cache_read_tokens,
        });
    }

    pub(super) fn observe_warmup(&mut self, usage: &TokenUsage) {
        self.observe_rates(usage);
        push_sample(
            &mut self.output,
            (usage.output_tokens as f64, usage.reasoning_tokens as f64),
        );
    }

    fn observe_rates(&mut self, usage: &TokenUsage) {
        if let Some(rates) = usage.cost.as_ref().and_then(|cost| cost.rates.as_ref()) {
            self.rates = Some(rates.clone());
        }
    }

    pub(super) fn has_no_discount(&self) -> bool {
        self.rates.as_ref().is_some_and(|rates| {
            rates
                .cache_read
                .is_some_and(|cached| cached.0 >= rates.input.0)
        })
    }

    pub(super) fn predicted_prefix(&self, prefix: &Prefix) -> Option<f64> {
        let last = self.latest.as_ref()?;
        let tokens = last.prefix_tokens?;
        Some(
            tokens
                + if *prefix == last.prefix {
                    0.0
                } else {
                    self.mean_growth()?
                },
        )
    }

    pub(super) fn uncached_tokens(&self, prefix: &Prefix, expired: bool) -> Option<f64> {
        let tokens = self.predicted_prefix(prefix)?;
        let last = self.latest.as_ref()?;
        let cached = if expired {
            0.0
        } else {
            (last.cached as f64).min(last.prefix_tokens?)
        };
        Some((tokens - cached).max(0.0))
    }

    pub(super) fn payback(
        &self,
        policy: CacheWarmingPolicy,
        prefix: &Prefix,
        expired: bool,
    ) -> Option<Payback> {
        let rates = self.rates.as_ref()?;
        let cached = rates.cache_read?.0 as f64;
        let saving = rates.input.0 as f64 - cached;
        let uncached = self.uncached_tokens(prefix, expired)?;
        Some(Payback {
            warmup_cost: cached * self.predicted_prefix(prefix)?
                + saving * uncached
                + self.output_cost(rates)?,
            expected_saving: policy.payback_requests as f64
                * CACHE_REUSE_PROBABILITY
                * saving
                * uncached,
        })
    }

    fn output_cost(&self, rates: &TokenRates) -> Option<f64> {
        (!self.output.is_empty()).then(|| {
            self.output
                .iter()
                .map(|(output, reasoning)| {
                    output * rates.output.0 as f64
                        + reasoning * rates.reasoning.unwrap_or(rates.output).0 as f64
                })
                .sum::<f64>()
                / self.output.len() as f64
        })
    }

    fn mean_growth(&self) -> Option<f64> {
        (!self.growth.is_empty())
            .then(|| self.growth.iter().sum::<f64>() / self.growth.len() as f64)
    }

    pub(super) fn interval(&self, policy: CacheWarmingPolicy, prefix: &Prefix) -> usize {
        let calculate = || {
            let rates = self.rates.as_ref()?;
            let cached = rates.cache_read?.0 as f64;
            let saving = rates.input.0 as f64 - cached;
            let growth = self.mean_growth()?;
            if saving <= 0.0 || growth <= 0.0 {
                return None;
            }
            let output_cost = self.output_cost(rates)?;
            let warmup = cached * self.predicted_prefix(prefix)? + output_cost;
            let optimum = (2.0 * warmup / (saving * growth)).sqrt();
            let lower = (optimum.floor() as usize).max(policy.min_requests_between_warmups);
            [lower, lower.saturating_add(1)]
                .into_iter()
                .map(|interval| {
                    let k = interval as f64;
                    (interval, warmup / k + saving * growth * (k - 1.0) / 2.0)
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(interval, _)| interval)
        };
        calculate().unwrap_or(policy.fallback_requests_between_warmups)
    }
}

fn push_sample<T>(samples: &mut VecDeque<T>, value: T) {
    samples.push_back(value);
    if samples.len() > SAMPLE_COUNT {
        samples.pop_front();
    }
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible test fixtures use direct assertions"
)]
mod tests;
