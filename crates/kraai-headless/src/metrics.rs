use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const HARNESS_METRICS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageMetrics {
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
}

impl UsageMetrics {
    #[must_use]
    pub fn used_context_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.reasoning_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    pub fn accumulate(&mut self, other: &Self) {
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(other.reasoning_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(other.cache_write_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
    }
}

impl From<&kraai_types::TokenUsage> for UsageMetrics {
    fn from(usage: &kraai_types::TokenUsage) -> Self {
        let count = |tokens: usize| u64::try_from(tokens).unwrap_or(u64::MAX);
        Self {
            total_tokens: count(usage.total_tokens),
            input_tokens: count(usage.input_tokens),
            output_tokens: count(usage.output_tokens),
            reasoning_tokens: count(usage.reasoning_tokens),
            cache_read_tokens: count(usage.cache_read_tokens),
            cache_write_tokens: count(usage.cache_write_tokens),
        }
    }
}

/// What a harness reports about one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessMetrics {
    pub schema_version: u32,
    /// Responses the model completed.
    #[serde(default)]
    pub turns: Option<u64>,
    #[serde(default)]
    pub script_executions: Option<u64>,
    #[serde(default)]
    pub final_context_tokens: Option<u64>,
    #[serde(default)]
    pub usage: Option<UsageMetrics>,
    #[serde(default)]
    pub request_costs: BTreeMap<kraai_types::MessageId, kraai_types::RequestUsage>,
    #[serde(default)]
    pub request_costs_complete: bool,
}
