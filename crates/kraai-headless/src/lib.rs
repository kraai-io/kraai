#![forbid(unsafe_code)]
//! Runs one prompt without a user interface and reports what the run did, for evaluations.

mod metrics;
#[cfg(feature = "runner")]
mod runner;

pub use metrics::{HARNESS_METRICS_SCHEMA_VERSION, HarnessMetrics, UsageMetrics};
#[cfg(feature = "runner")]
pub use runner::{RunOptions, RunOutcome, run};
