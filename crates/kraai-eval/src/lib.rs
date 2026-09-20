#![forbid(unsafe_code)]
#![deny(clippy::all)]

mod accounting;
mod benchmark;
mod cache;
mod cargo_dependencies;
mod command;
mod comparison;
mod containers;
mod event_log;
mod execution;
mod harbor;
mod harness;
mod manifest;
mod metrics;
mod progress;
mod provider_config;
mod proxy;
mod records;
mod runner;
mod sandbox;
mod suite;
mod validation;
mod workspace;

pub use accounting::{
    AccountingSummary, ContextMetrics, PairedRequestMetrics, PricingOptions, RequestAccounting,
    analyze_requests, load_accounting,
};
pub use benchmark::{BenchmarkSpec, prepare_benchmark_spec, runner_store_root};
pub use cache::{
    ExperimentIdentity, ResultStore, RunCoordinates, hash_file, load_run_result, path_segment,
};
pub use comparison::{
    ComparedRun, ComparisonResult, ComparisonSuite, EfficiencyMetrics, PairOutcome, PairedMetric,
    PairedUsageMetrics, compare, compare_with_cache_roots,
};
pub use containers::{benchmark_environment, docker_proxy_host};
pub use harbor::{HarborComparisonResult, compare_harbor_jobs, format_harbor_comparison};
pub use harness::{HarnessProfile, ProxyKind, ResolvedHarness};
pub use manifest::{CommandSpec, NetworkPolicy, TaskManifest};
pub use metrics::{EvaluationMetrics, HarnessMetrics, ProxyMetrics, UsageMetrics};
pub use provider_config::KraaiProviderConfigRequest;
pub use proxy::ModelProxyRequest;
pub use proxy::service::{ProxyServiceRequest, serve_model_proxy};
pub use suite::{SuiteRequest, SuiteResult, run_suite};
pub use validation::{MutationValidation, TaskValidation, validate_task};

pub use progress::{ProgressReporter, ProgressSnapshot};
pub use records::{
    ControllerFailure, ProcessRecord, ProxyRecord, RunResult, RunStatus, SandboxRecord,
};
pub use runner::{RunRequest, run};

#[cfg(test)]
fn eval_assets_directory() -> std::path::PathBuf {
    std::env::var_os("KRAAI_EVAL_ASSETS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals"))
}
