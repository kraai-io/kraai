use std::fs;
use std::path::Path;

use color_eyre::eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageMetrics {
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
}

impl UsageMetrics {
    pub fn used_context_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.reasoning_tokens)
            .saturating_add(self.cache_read_tokens)
    }

    pub(crate) fn accumulate(&mut self, other: &Self) {
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(other.reasoning_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessMetrics {
    pub schema_version: u32,
    #[serde(default)]
    pub turns: Option<u64>,
    #[serde(default)]
    pub tool_calls: Option<u64>,
    #[serde(default)]
    pub final_context_tokens: Option<u64>,
    #[serde(default)]
    pub usage: Option<UsageMetrics>,
}

impl HarnessMetrics {
    pub(crate) fn load(path: &Path) -> Result<Option<Self>> {
        let contents =
            fs::read(path).wrap_err_with(|| format!("read harness metrics {}", path.display()))?;
        if contents.iter().all(u8::is_ascii_whitespace) {
            return Ok(None);
        }
        let metrics: Self = serde_json::from_slice(&contents)
            .wrap_err_with(|| format!("parse harness metrics {}", path.display()))?;
        if metrics.schema_version != 1 {
            bail!(
                "unsupported harness metrics schema version {}",
                metrics.schema_version
            );
        }
        Ok(Some(metrics))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyMetrics {
    pub requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub duration_ms: u128,
    pub usage: UsageMetrics,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessMetrics>,
}

impl EvaluationMetrics {
    pub fn usage(&self) -> Option<&UsageMetrics> {
        self.proxy
            .as_ref()
            .map(|metrics| &metrics.usage)
            .filter(|usage| **usage != UsageMetrics::default())
            .or_else(|| {
                self.harness
                    .as_ref()
                    .and_then(|metrics| metrics.usage.as_ref())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_usage_is_authoritative_when_both_sources_exist() {
        let proxy_usage = UsageMetrics {
            total_tokens: 10,
            ..UsageMetrics::default()
        };
        let harness_usage = UsageMetrics {
            total_tokens: 20,
            ..UsageMetrics::default()
        };
        let metrics = EvaluationMetrics {
            proxy: Some(ProxyMetrics {
                usage: proxy_usage.clone(),
                ..ProxyMetrics::default()
            }),
            harness: Some(HarnessMetrics {
                schema_version: 1,
                turns: None,
                tool_calls: None,
                final_context_tokens: None,
                usage: Some(harness_usage),
            }),
        };
        assert_eq!(metrics.usage(), Some(&proxy_usage));
    }
}
