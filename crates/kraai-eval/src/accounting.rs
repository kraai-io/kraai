mod comparison;
mod measurement;
mod pricing;
pub use comparison::PairedRequestMetrics;
pub use measurement::RequestMeasurement;

use std::fs;
use std::path::Path;

use color_eyre::eyre::{Result, ensure};
use kraai_types::Usd;
use serde::{Deserialize, Serialize};

pub use pricing::PricingOptions;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextMetrics {
    pub samples: u64,
    pub total_input_tokens: u128,
    pub min_input_tokens: Option<u64>,
    pub peak_input_tokens: Option<u64>,
    pub last_input_tokens: Option<u64>,
    pub mean_input_tokens: Option<f64>,
}

impl ContextMetrics {
    fn record(&mut self, input: u64) {
        self.samples += 1;
        self.total_input_tokens += u128::from(input);
        self.min_input_tokens = Some(
            self.min_input_tokens
                .map_or(input, |previous| previous.min(input)),
        );
        self.peak_input_tokens = Some(
            self.peak_input_tokens
                .map_or(input, |previous| previous.max(input)),
        );
        self.last_input_tokens = Some(input);
        self.mean_input_tokens = Some(self.total_input_tokens as f64 / self.samples as f64);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestAccounting {
    pub events_sha256: String,
    pub model_requests: u64,
    pub unrecorded_requests: u64,
    pub context: ContextMetrics,
    pub known_estimated_cost: Usd,
    pub priced_requests: u64,
    pub unpriced_requests: u64,
    pub cost_overflow: bool,
    pub requests: Vec<RequestMeasurement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountingSummary {
    pub model_requests: u64,
    pub unrecorded_requests: u64,
    pub context: ContextMetrics,
    pub estimated_cost: Option<Usd>,
    pub known_estimated_cost: Usd,
    pub unpriced_requests: u64,
}

impl RequestAccounting {
    pub fn summary(&self) -> AccountingSummary {
        AccountingSummary {
            model_requests: self.model_requests,
            unrecorded_requests: self.unrecorded_requests,
            context: self.context.clone(),
            estimated_cost: self.complete_cost(),
            known_estimated_cost: self.known_estimated_cost,
            unpriced_requests: self.unpriced_requests,
        }
    }

    pub fn pricing_basis(&self) -> Option<std::collections::BTreeMap<&str, &str>> {
        let mut bases = std::collections::BTreeMap::new();
        for request in &self.requests {
            let model = request.model.as_deref()?;
            let basis = request.pricing_basis.as_deref()?;
            if bases
                .insert(model, basis)
                .is_some_and(|previous| previous != basis)
            {
                return None;
            }
        }
        (!bases.is_empty()).then_some(bases)
    }

    pub fn complete_cost(&self) -> Option<Usd> {
        (self.model_requests > 0
            && self.unpriced_requests == 0
            && self.unrecorded_requests == 0
            && !self.cost_overflow)
            .then_some(self.known_estimated_cost)
    }

    pub fn complete_context(&self) -> Option<&ContextMetrics> {
        (self.model_requests > 0
            && self.context.samples == self.model_requests
            && self.unrecorded_requests == 0)
            .then_some(&self.context)
    }
}

pub fn analyze_requests(
    path: &Path,
    expected_requests: u64,
    options: &PricingOptions,
) -> Result<RequestAccounting> {
    let (text, events_sha256) = crate::cache::read_hashed_text_file(path)?;
    let events = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(text);
    let worker_options = options.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let pricing = if events.iter().any(|event| {
                    event.get("model").is_some_and(serde_json::Value::is_string)
                        && event.get("usage").is_some_and(serde_json::Value::is_object)
                }) {
                    Some(pricing::RequestPricing::load(&worker_options).await?)
                } else {
                    None
                };
                let mut result = RequestAccounting {
                    events_sha256,
                    unrecorded_requests: expected_requests.saturating_sub(events.len() as u64),
                    ..Default::default()
                };
                for (event_index, event) in events.into_iter().enumerate() {
                    let Some(mut measurement) = RequestMeasurement::from_event(event_index, event)?
                    else {
                        continue;
                    };
                    let input = measurement.input_context_tokens;
                    let estimate = match &pricing {
                        Some(pricing) => pricing.estimate(&measurement).await,
                        None => Err(String::from("missing actual model or request usage")),
                    };
                    match estimate {
                        Ok((cost, basis)) => {
                            result.priced_requests += 1;
                            if let Some(sum) =
                                result.known_estimated_cost.0.checked_add(cost.amount.0)
                            {
                                result.known_estimated_cost = Usd(sum);
                            } else {
                                result.cost_overflow = true;
                            }
                            measurement.estimated_cost = Some(cost);
                            measurement.pricing_basis = Some(basis);
                        }
                        Err(reason) => {
                            result.unpriced_requests += 1;
                            measurement.unpriced_reason = Some(reason);
                        }
                    }
                    result.model_requests += 1;
                    if let Some(input) = input {
                        result.context.record(input);
                    }
                    result.context.last_input_tokens = input;
                    result.requests.push(measurement);
                }
                if result.unrecorded_requests != 0 {
                    result.context.last_input_tokens = None;
                }
                Ok(result)
            })
    })
    .join()
    .map_err(|_panic| color_eyre::eyre::eyre!("request accounting worker panicked"))?
}

pub fn load_accounting(path: &Path) -> Result<Option<RequestAccounting>> {
    if !path.is_file() {
        return Ok(None);
    }
    let accounting: RequestAccounting = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        accounting.events_sha256
            == crate::cache::hash_file(&path.with_file_name("proxy.events.jsonl"))?,
        "request accounting does not match its proxy events"
    );
    Ok(Some(accounting))
}

#[cfg(test)]
mod tests;
