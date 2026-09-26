mod trajectory;

use std::path::Path;

use color_eyre::eyre::{Result, ensure};
use serde_json::Value;

use super::{Catalog, Metrics, files};
use crate::{EvaluationMetrics, HarnessMetrics, ProxyMetrics, RequestAccounting};

impl Catalog {
    pub(super) fn hydrate_proxy(&mut self, directory: &Path, proxy: &mut ProxyMetrics) {
        let path = directory.join("request-accounting.json");
        if !path.exists() {
            return;
        }
        let result = (|| -> Result<RequestAccounting> {
            let accounting: RequestAccounting = files::read_json(&self.root, &path)?;
            let (events, truncated) = files::read_bounded(
                &self.root,
                &directory.join("proxy.events.jsonl"),
                files::JSON_LIMIT,
            )?;
            ensure!(!truncated, "proxy events exceed the 8 MiB viewer limit");
            ensure!(
                accounting.events_sha256 == crate::cache::hash_chunks(&[events]),
                "saved accounting does not match proxy events"
            );
            Ok(accounting)
        })();
        match result {
            Ok(accounting) => {
                proxy.accounting = Some(accounting);
                proxy.accounting_error = None;
            }
            Err(error) => {
                self.warning(&path, format!("{error:#}"));
                proxy.accounting = None;
                proxy.accounting_error = Some(error.to_string());
            }
        }
    }

    pub(super) fn native_metrics(
        &mut self,
        directory: &Path,
        saved: &EvaluationMetrics,
    ) -> Metrics {
        let mut metrics = Metrics::default();
        if let Some(usage) = saved.usage() {
            if usage.total_tokens == usage.used_context_tokens() {
                metrics.input_tokens = Some(
                    u128::from(usage.input_tokens)
                        + u128::from(usage.cache_read_tokens)
                        + u128::from(usage.cache_write_tokens),
                );
                metrics.cached_input_tokens = Some(u128::from(usage.cache_read_tokens));
                metrics.uncached_input_tokens = Some(u128::from(usage.input_tokens));
                metrics.output_tokens =
                    Some(u128::from(usage.output_tokens) + u128::from(usage.reasoning_tokens));
                metrics.reasoning_tokens = Some(u128::from(usage.reasoning_tokens));
            } else {
                self.warning(directory, "saved token subdivisions disagree with total");
            }
        }
        metrics.requests = saved.proxy.as_ref().map(requests);
        let accounting = saved
            .proxy
            .as_ref()
            .filter(|proxy| proxy.accounting_error.is_none() && proxy.unrecorded_requests == 0)
            .and_then(|proxy| proxy.accounting.as_ref());
        metrics.final_context_tokens = accounting
            .and_then(RequestAccounting::complete_context)
            .and_then(|context| context.last_input_tokens)
            .map(u128::from);
        metrics.cost_usd = accounting
            .and_then(RequestAccounting::complete_cost)
            .map(|cost| cost.0 as f64 / 1_000_000_000.0);
        metrics.harness(saved.harness.as_ref());
        if saved
            .proxy
            .as_ref()
            .is_none_or(|proxy| proxy.accounting_error.is_none() && proxy.unrecorded_requests == 0)
            && saved
                .proxy
                .as_ref()
                .is_none_or(|proxy| proxy.accounting.is_none())
        {
            metrics.cost_usd = saved.harness.as_ref().and_then(harness_cost);
        }
        metrics
    }

    pub(super) fn harbor_metrics(
        &mut self,
        directory: &Path,
        result: &Value,
        adapter: bool,
    ) -> Metrics {
        let controller = directory.join("kraai-controller");
        let proxy_path = controller.join("proxy-metrics.json");
        let mut proxy: Option<ProxyMetrics> = self.optional_json(&proxy_path);
        if proxy.is_none() && !proxy_path.exists() {
            proxy = result
                .pointer("/agent_result/metadata/kraai_eval/proxy")
                .filter(|value| !value.is_null())
                .and_then(|value| match serde_json::from_value(value.clone()) {
                    Ok(proxy) => Some(proxy),
                    Err(error) => {
                        self.warning(directory, error);
                        None
                    }
                });
        }
        if let Some(proxy) = proxy.as_mut() {
            self.hydrate_proxy(&controller, proxy);
        }
        let request_count = proxy.as_ref().map(requests);
        let accounting_usable = proxy
            .as_ref()
            .is_none_or(|proxy| proxy.accounting_error.is_none() && proxy.unrecorded_requests == 0);
        let allow_harbor = !adapter && !proxy_path.exists() && proxy.is_none();
        let normalized = crate::harbor::metrics::trial_metrics(result, proxy, allow_harbor);
        let mut metrics = match normalized {
            Ok(normalized) => Metrics {
                input_tokens: normalized.input_tokens,
                cached_input_tokens: normalized.cache_read_tokens,
                uncached_input_tokens: normalized.uncached_input_tokens,
                output_tokens: normalized.output_tokens,
                reasoning_tokens: normalized.reasoning_tokens,
                requests: request_count,
                duration_ms: normalized.wall_time_ms,
                final_context_tokens: accounting_usable
                    .then(|| {
                        normalized
                            .accounting
                            .as_ref()
                            .and_then(RequestAccounting::complete_context)
                            .and_then(|context| context.last_input_tokens)
                            .map(u128::from)
                    })
                    .flatten(),
                cost_usd: normalized
                    .accounting
                    .as_ref()
                    .filter(|_| accounting_usable)
                    .and_then(RequestAccounting::complete_cost)
                    .map(|cost| cost.0 as f64 / 1_000_000_000.0)
                    .or_else(|| {
                        allow_harbor
                            .then(|| {
                                result
                                    .pointer("/agent_result/cost_usd")
                                    .and_then(Value::as_f64)
                                    .filter(|cost| cost.is_finite() && *cost >= 0.0)
                            })
                            .flatten()
                    }),
                ..Metrics::default()
            },
            Err(error) => {
                self.warning(directory, error);
                Metrics::default()
            }
        };
        let harness = result
            .pointer("/agent_result/metadata/kraai_eval/harness")
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::from_value::<HarnessMetrics>(value.clone()).ok())
            .or_else(|| self.optional_json(&directory.join("agent/kraai-metrics.json")));
        metrics.harness(harness.as_ref());
        if allow_harbor {
            self.hydrate_trajectory(directory, result, &mut metrics);
        }
        metrics
    }
}

impl Metrics {
    fn harness(&mut self, harness: Option<&HarnessMetrics>) {
        if let Some(harness) = harness.filter(|harness| harness.schema_version == 1) {
            self.turns = harness.turns.map(u128::from);
        }
    }
}

fn requests(proxy: &ProxyMetrics) -> u128 {
    proxy.accounting.as_ref().map_or_else(
        || u128::from(proxy.requests) + u128::from(proxy.unrecorded_requests),
        |accounting| {
            u128::from(accounting.model_requests) + u128::from(accounting.unrecorded_requests)
        },
    )
}

fn harness_cost(harness: &HarnessMetrics) -> Option<f64> {
    if !harness.request_costs_complete || harness.request_costs.is_empty() {
        return None;
    }
    harness
        .request_costs
        .values()
        .try_fold(0_u64, |total, request| {
            if request.unpriced_attempts != 0 {
                return None;
            }
            total.checked_add(request.usage.as_ref()?.cost.as_ref()?.amount.0)
        })
        .map(|cost| cost as f64 / 1_000_000_000.0)
}
