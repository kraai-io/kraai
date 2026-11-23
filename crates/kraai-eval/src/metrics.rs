use std::fs;
use std::path::Path;

use color_eyre::eyre::{Context, Result, bail};
use kraai_io::read::{ReadLimitError, read_bounded};
use serde::{Deserialize, Serialize};

pub use kraai_headless::{HarnessMetrics, UsageMetrics};

const MAX_HARNESS_METRICS_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) fn load_harness_metrics(path: &Path) -> Result<Option<HarnessMetrics>> {
    let contents = fs::File::open(path)
        .map_err(ReadLimitError::from)
        .and_then(|file| read_bounded(file, MAX_HARNESS_METRICS_BYTES))
        .wrap_err_with(|| format!("read harness metrics {}", path.display()))?;
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
mod tests;
