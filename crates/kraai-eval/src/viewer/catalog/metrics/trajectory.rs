use std::path::Path;

use color_eyre::eyre::{Result, ensure};
use serde::Deserialize;
use serde_json::Value;

use super::super::{Catalog, Metrics, files};

#[derive(Deserialize)]
struct TrajectoryMetrics {
    schema_version: u32,
    trial_id: String,
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    final_context_tokens: Option<u64>,
    turns: Option<u64>,
    requests: Option<u64>,
}

impl TrajectoryMetrics {
    fn validate(&self, result: &Value, metrics: &Metrics) -> Result<()> {
        ensure!(
            self.schema_version == 1,
            "unsupported trajectory metrics schema"
        );
        ensure!(
            result.get("id").and_then(Value::as_str) == Some(self.trial_id.as_str()),
            "trajectory metrics do not match the saved trial"
        );
        for (name, saved, normalized) in [
            ("input", self.input_tokens, metrics.input_tokens),
            (
                "cached input",
                self.cached_input_tokens,
                metrics.cached_input_tokens,
            ),
            ("output", self.output_tokens, metrics.output_tokens),
        ] {
            ensure!(
                saved.map(u128::from) == normalized,
                "trajectory {name} tokens do not match the saved trial"
            );
        }
        if let (Some(reasoning), Some(output)) = (self.reasoning_tokens, self.output_tokens) {
            ensure!(
                reasoning <= output,
                "trajectory reasoning exceeds output tokens"
            );
        }
        if let (Some(context), Some(input)) = (self.final_context_tokens, self.input_tokens) {
            ensure!(
                context <= input,
                "trajectory final context exceeds input tokens"
            );
        }
        Ok(())
    }
}

impl Catalog {
    pub(super) fn hydrate_trajectory(
        &mut self,
        directory: &Path,
        result: &Value,
        metrics: &mut Metrics,
    ) {
        let path = directory.join("agent/trajectory-metrics.json");
        if !path.exists() {
            return;
        }
        let supplemental = (|| -> Result<TrajectoryMetrics> {
            let supplemental: TrajectoryMetrics = files::read_json(&self.root, &path)?;
            supplemental.validate(result, metrics)?;
            Ok(supplemental)
        })();
        match supplemental {
            Ok(supplemental) => {
                metrics.reasoning_tokens = supplemental.reasoning_tokens.map(u128::from);
                metrics.final_context_tokens = supplemental.final_context_tokens.map(u128::from);
                metrics.turns = supplemental.turns.map(u128::from);
                metrics.requests = supplemental.requests.map(u128::from);
            }
            Err(error) => self.warning(&path, format!("{error:#}")),
        }
    }
}
