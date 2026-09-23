use std::collections::VecDeque;

use kraai_types::{TokenRates, TokenUsage};

use super::{CacheWarmingPolicy, CompletedWarmup, prefix::Prefix};

const SAMPLE_COUNT: usize = 5;

#[derive(Default)]
pub(super) struct Feedback {
    rates: Option<TokenRates>,
    growth: VecDeque<f64>,
    output: VecDeque<(f64, f64)>,
    latest: Option<Measurement>,
    pinned_tokens: Option<usize>,
}

struct Measurement {
    prefix: Prefix,
    suffix: Vec<u64>,
    input: usize,
}

pub(super) fn input_tokens(usage: &TokenUsage) -> usize {
    usage
        .input_tokens
        .saturating_add(usage.cache_read_tokens)
        .saturating_add(usage.cache_write_tokens)
}

impl Feedback {
    pub(super) fn synchronize(&mut self, prefix: &Prefix, suffix: &[u64]) {
        if self
            .latest
            .as_ref()
            .is_some_and(|last| !prefix.extends(&last.prefix) || suffix != last.suffix)
        {
            self.latest = None;
            self.growth.clear();
            self.pinned_tokens = None;
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
        self.synchronize(&prefix, &suffix);
        let input = input_tokens(usage);
        if let Some(warmup) = completed.filter(|warmup| warmup.prefix == prefix) {
            self.pinned_tokens = input.checked_sub(warmup.input_tokens);
        }
        if let Some(last) = &self.latest {
            if last.prefix == prefix {
                return;
            }
            if let Some(growth) = input.checked_sub(last.input) {
                push_sample(&mut self.growth, growth as f64);
            } else {
                self.growth.clear();
                self.pinned_tokens = None;
            }
        }
        self.latest = Some(Measurement {
            prefix,
            suffix,
            input,
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
        let tokens = last.input.checked_sub(self.pinned_tokens?)? as f64;
        Some(
            tokens
                + if *prefix == last.prefix {
                    0.0
                } else {
                    self.mean_growth()?
                },
        )
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
            if saving <= 0.0 || growth <= 0.0 || self.output.is_empty() {
                return None;
            }
            let output_cost = self
                .output
                .iter()
                .map(|(output, reasoning)| {
                    output * rates.output.0 as f64
                        + reasoning * rates.reasoning.unwrap_or(rates.output).0 as f64
                })
                .sum::<f64>()
                / self.output.len() as f64;
            let warmup = cached * self.predicted_prefix(prefix)? + output_cost;
            (policy.min_requests_between_warmups..=policy.max_requests_between_warmups)
                .map(|interval| {
                    let k = interval as f64;
                    (interval, warmup / k + saving * growth * (k - 1.0) / 2.0)
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(interval, _)| interval)
        };
        calculate().unwrap_or(policy.max_requests_between_warmups)
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
