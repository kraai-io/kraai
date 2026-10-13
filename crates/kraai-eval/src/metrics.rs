use std::fs;
use std::path::Path;

use color_eyre::eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub use kraai_headless::{HarnessMetrics, UsageMetrics};

pub(crate) fn load_harness_metrics(path: &Path) -> Result<Option<HarnessMetrics>> {
    let contents =
        fs::read(path).wrap_err_with(|| format!("read harness metrics {}", path.display()))?;
    if contents.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let metrics: HarnessMetrics = serde_json::from_slice(&contents)
        .wrap_err_with(|| format!("parse harness metrics {}", path.display()))?;
    if metrics.schema_version != kraai_headless::HARNESS_METRICS_SCHEMA_VERSION {
        bail!(
            "unsupported harness metrics schema version {}",
            metrics.schema_version
        );
    }
    Ok(Some(metrics))
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
                request_costs: std::collections::BTreeMap::new(),
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
        let metrics = load_harness_metrics(&path)?
            .ok_or_else(|| color_eyre::eyre::eyre!("native metrics were discarded"))?;
        color_eyre::eyre::ensure!(metrics.request_costs.len() == 1);
        color_eyre::eyre::ensure!(metrics.request_costs_complete);
        color_eyre::eyre::ensure!(serde_json::to_value(&metrics)? == emitted);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
