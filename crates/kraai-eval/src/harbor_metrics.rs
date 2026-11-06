use color_eyre::eyre::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ProxyMetrics;

trait JsonField {
    fn field(&self, pointer: &str) -> &Value;
}

impl JsonField for Value {
    fn field(&self, pointer: &str) -> &Value {
        self.pointer(pointer).unwrap_or(&Self::Null)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HarborTrialMetrics {
    pub wall_time_ms: Option<u128>,
    pub runner_time_ms: Option<u128>,
    pub total_tokens: Option<u128>,
    pub input_tokens: Option<u128>,
    pub uncached_input_tokens: Option<u128>,
    pub cache_read_tokens: Option<u128>,
    pub output_tokens: Option<u128>,
    pub reasoning_tokens: Option<u128>,
    pub proxy_requests: Option<u128>,
    pub usage_source: String,
    #[serde(default)]
    pub accounting: Option<crate::RequestAccounting>,
}

pub(crate) fn trial_metrics(
    result: &Value,
    proxy: Option<ProxyMetrics>,
    allow_harbor_usage: bool,
) -> Result<HarborTrialMetrics> {
    let context = if proxy.is_some() || !allow_harbor_usage {
        &Value::Null
    } else {
        result.field("/agent_result")
    };
    let mut metrics = HarborTrialMetrics {
        wall_time_ms: duration(result.field("/started_at"), result.field("/finished_at"))?,
        runner_time_ms: duration(
            result.field("/agent_execution/started_at"),
            result.field("/agent_execution/finished_at"),
        )?,
        input_tokens: count(context.field("/n_input_tokens"))?,
        cache_read_tokens: count(context.field("/n_cache_tokens"))?,
        output_tokens: count(context.field("/n_output_tokens"))?,
        usage_source: String::from(if allow_harbor_usage {
            "harbor_agent_result"
        } else {
            "controller_proxy_usage_unavailable"
        }),
        ..HarborTrialMetrics::default()
    };
    if let Some(proxy) = proxy {
        let usage_complete = proxy.unrecorded_requests == 0
            && proxy.accounting_error.is_none()
            && proxy.accounting.as_ref().is_none_or(|accounting| {
                accounting.complete_context().is_some()
                    || (proxy.requests == 0 && accounting.unrecorded_requests == 0)
            });
        metrics.accounting = proxy.accounting;
        let usage = proxy.usage;
        metrics.proxy_requests = Some(u128::from(proxy.requests));
        if usage_complete && (usage != crate::UsageMetrics::default() || proxy.requests == 0) {
            ensure!(
                usage.total_tokens == usage.used_context_tokens(),
                "proxy token subdivisions do not match total"
            );
            metrics.input_tokens = Some(
                u128::from(usage.input_tokens)
                    + u128::from(usage.cache_read_tokens)
                    + u128::from(usage.cache_write_tokens),
            );
            metrics.uncached_input_tokens = Some(u128::from(usage.input_tokens));
            metrics.cache_read_tokens = Some(u128::from(usage.cache_read_tokens));
            metrics.output_tokens =
                Some(u128::from(usage.output_tokens) + u128::from(usage.reasoning_tokens));
            metrics.reasoning_tokens = Some(u128::from(usage.reasoning_tokens));
            metrics.usage_source = String::from("controller_proxy");
        } else if usage_complete {
            metrics.usage_source = String::from("controller_proxy_usage_unavailable");
        } else {
            metrics.usage_source = String::from("controller_proxy_usage_incomplete");
        }
    }
    if let (Some(input), Some(cached)) = (metrics.input_tokens, metrics.cache_read_tokens) {
        ensure!(cached <= input, "cached tokens exceed total input tokens");
        metrics.uncached_input_tokens.get_or_insert(input - cached);
    }
    metrics.total_tokens = metrics
        .input_tokens
        .zip(metrics.output_tokens)
        .map(|(input, output)| input + output);
    Ok(metrics)
}

fn count(value: &Value) -> Result<Option<u128>> {
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(u128::from(value.as_u64().ok_or_else(|| {
        color_eyre::eyre::eyre!("invalid Harbor token count")
    })?)))
}

fn duration(start: &Value, end: &Value) -> Result<Option<u128>> {
    let parse = |value: &Value| -> Result<Option<chrono::DateTime<chrono::FixedOffset>>> {
        if value.is_null() {
            return Ok(None);
        }
        Ok(Some(chrono::DateTime::parse_from_rfc3339(
            value
                .as_str()
                .ok_or_else(|| color_eyre::eyre::eyre!("invalid Harbor timestamp"))?,
        )?))
    };
    match (parse(start)?, parse(end)?) {
        (Some(start), Some(end)) => {
            let elapsed = end.signed_duration_since(start).num_milliseconds();
            ensure!(elapsed >= 0, "Harbor timestamps run backwards");
            Ok(Some(elapsed as u128))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_cache_writes_contribute_to_total_input_without_becoming_uncached_input() -> Result<()>
    {
        let metrics = trial_metrics(
            &Value::Null,
            Some(ProxyMetrics {
                requests: 1,
                usage: crate::UsageMetrics {
                    total_tokens: 70,
                    input_tokens: 10,
                    cache_read_tokens: 20,
                    cache_write_tokens: 30,
                    output_tokens: 4,
                    reasoning_tokens: 6,
                },
                ..Default::default()
            }),
            false,
        )?;
        ensure!(metrics.input_tokens == Some(60));
        ensure!(metrics.uncached_input_tokens == Some(10));
        ensure!(metrics.cache_read_tokens == Some(20));
        ensure!(metrics.output_tokens == Some(10));
        ensure!(metrics.reasoning_tokens == Some(6));
        ensure!(metrics.total_tokens == Some(70));
        Ok(())
    }

    #[test]
    fn incomplete_request_accounting_preserves_known_usage_without_pairing_partial_totals()
    -> Result<()> {
        for (samples, unrecorded, error) in [(1, 0, None), (2, 1, None), (2, 0, Some("failed"))] {
            let metrics = trial_metrics(
                &Value::Null,
                Some(ProxyMetrics {
                    requests: 2,
                    accounting_error: error.map(str::to_owned),
                    usage: crate::UsageMetrics {
                        total_tokens: 15,
                        input_tokens: 10,
                        output_tokens: 5,
                        ..Default::default()
                    },
                    accounting: Some(crate::RequestAccounting {
                        model_requests: 2,
                        unrecorded_requests: unrecorded,
                        context: crate::ContextMetrics {
                            samples,
                            total_input_tokens: 10,
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                false,
            )?;
            ensure!(metrics.total_tokens.is_none());
            ensure!(metrics.input_tokens.is_none());
            ensure!(metrics.output_tokens.is_none());
            ensure!(metrics.proxy_requests == Some(2));
            ensure!(metrics.usage_source == "controller_proxy_usage_incomplete");
            ensure!(metrics.accounting.as_ref().is_some_and(|accounting| {
                accounting.context.samples == samples && accounting.context.total_input_tokens == 10
            }));
        }
        Ok(())
    }

    #[test]
    fn unrecorded_requests_without_accounting_never_supply_complete_token_totals() -> Result<()> {
        let metrics = trial_metrics(
            &Value::Null,
            Some(ProxyMetrics {
                requests: 1,
                unrecorded_requests: 1,
                usage: crate::UsageMetrics {
                    input_tokens: 10,
                    total_tokens: 10,
                    ..Default::default()
                },
                ..Default::default()
            }),
            false,
        )?;
        ensure!(metrics.total_tokens.is_none());
        Ok(())
    }
}
