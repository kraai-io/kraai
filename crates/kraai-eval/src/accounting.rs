mod measurement;
pub use measurement::RequestMeasurement;

use std::fs;
use std::path::Path;

use color_eyre::eyre::{Result, ensure};
use serde::{Deserialize, Serialize};

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
    pub requests: Vec<RequestMeasurement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountingSummary {
    pub model_requests: u64,
    pub unrecorded_requests: u64,
    pub context: ContextMetrics,
}

impl RequestAccounting {
    pub fn summary(&self) -> AccountingSummary {
        AccountingSummary {
            model_requests: self.model_requests,
            unrecorded_requests: self.unrecorded_requests,
            context: self.context.clone(),
        }
    }

    pub fn complete_context(&self) -> Option<&ContextMetrics> {
        (self.model_requests > 0
            && self.context.samples == self.model_requests
            && self.unrecorded_requests == 0)
            .then_some(&self.context)
    }
}

pub fn analyze_requests(path: &Path, expected_requests: u64) -> Result<RequestAccounting> {
    let (text, events_sha256) = crate::cache::read_hashed_text_file(path)?;
    let mut result = RequestAccounting {
        events_sha256,
        ..Default::default()
    };
    let mut events = 0_u64;
    for (event_index, line) in text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        events += 1;
        let Some(measurement) =
            RequestMeasurement::from_event(event_index, serde_json::from_str(line)?)?
        else {
            continue;
        };
        let input = measurement.input_context_tokens;
        result.model_requests += 1;
        if let Some(input) = input {
            result.context.record(input);
        }
        result.context.last_input_tokens = input;
        result.requests.push(measurement);
    }
    result.unrecorded_requests = expected_requests.saturating_sub(events);
    if result.unrecorded_requests != 0 {
        result.context.last_input_tokens = None;
    }
    Ok(result)
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
