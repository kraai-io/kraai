use std::collections::BTreeMap;
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
    #[serde(default)]
    pub cache_write_tokens: u64,
}

impl UsageMetrics {
    pub fn used_context_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.reasoning_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    pub(crate) fn accumulate(&mut self, other: &Self) {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessMetrics {
    pub schema_version: u32,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyMetrics {
    pub requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    #[serde(default)]
    pub client_disconnects: u64,
    pub duration_ms: u128,
    pub usage: UsageMetrics,
    #[serde(default)]
    pub unrecorded_requests: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounting: Option<crate::RequestAccounting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounting_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvaluationMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessMetrics>,
}

impl EvaluationMetrics {
    pub fn usage(&self) -> Option<&UsageMetrics> {
        if self.proxy.as_ref().is_some_and(|proxy| {
            proxy.unrecorded_requests != 0
                || proxy.accounting_error.is_some()
                || proxy
                    .accounting
                    .as_ref()
                    .is_some_and(|accounting| accounting.complete_context().is_none())
        }) {
            return None;
        }

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
                request_costs: BTreeMap::new(),
                request_costs_complete: false,
                schema_version: 1,
                turns: None,
                script_executions: None,
                final_context_tokens: None,
                usage: Some(harness_usage),
            }),
        };
        assert_eq!(metrics.usage(), Some(&proxy_usage));
    }

    #[test]
    fn proxy_metrics_default_client_disconnects_when_loading_older_results() -> Result<()> {
        let metrics: ProxyMetrics = serde_json::from_str(
            r#"{"requests":1,"successful_requests":1,"failed_requests":0,"duration_ms":2,"usage":{"total_tokens":3,"input_tokens":2,"output_tokens":1,"reasoning_tokens":0,"cache_read_tokens":0}}"#,
        )?;

        color_eyre::eyre::ensure!(metrics.client_disconnects == 0);
        Ok(())
    }

    #[test]
    fn loads_native_request_costs_keyed_by_message_id() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "kraai-eval-native-metrics-{}",
            ulid::Ulid::generate()
        ));
        fs::create_dir(&root)?;
        let path = root.join("harness-metrics.json");
        let emitted = serde_json::json!({
            "schema_version": 1,
            "turns": 1,
            "script_executions": 1,
            "final_context_tokens": 2243,
            "usage": {
                "total_tokens": 2243, "input_tokens": 2108, "output_tokens": 135,
                "reasoning_tokens": 0, "cache_read_tokens": 0, "cache_write_tokens": 0
            },
            "request_costs": {
                "request": {
                    "message_id": "request", "provider_id": "openai-codex",
                    "model_id": "gpt-5.6-sol-low", "started_at": 1789315143614_u64,
                    "subscription": true, "unpriced_attempts": 0,
                    "usage": {
                        "total_tokens": 2243, "input_tokens": 2108, "output_tokens": 135,
                        "reasoning_tokens": 0, "cache_read_tokens": 0, "cache_write_tokens": 0,
                        "cost": {
                            "amount": 11132000, "source": "models.dev/openai/gpt-5.6-sol",
                            "priced_at": 1789315143, "upstream": null,
                            "rates": {
                                "input": 4000000000_u64, "output": 20000000000_u64,
                                "cache_read": 400000000, "cache_write": 5000000000_u64,
                                "reasoning": null
                            }
                        }
                    }
                }
            },
            "request_costs_complete": true
        });
        fs::write(&path, serde_json::to_vec(&emitted)?)?;
        let metrics = HarnessMetrics::load(&path)?
            .ok_or_else(|| color_eyre::eyre::eyre!("native metrics were discarded"))?;
        color_eyre::eyre::ensure!(metrics.request_costs.len() == 1);
        color_eyre::eyre::ensure!(metrics.request_costs_complete);
        color_eyre::eyre::ensure!(serde_json::to_value(&metrics)? == emitted);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
