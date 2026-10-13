#![forbid(unsafe_code)]

use std::path::PathBuf;

use clap::Parser;
use color_eyre::eyre::{Result, eyre};
use kraai_headless::{RunOptions, RunOutcome};
use kraai_runtime::RuntimeBuilder;

/// Runs one prompt to completion without a user interface. Writes the assistant's output to
/// stdout and, when `KRAAI_EVAL_METRICS_PATH` is set, the run's metrics there.
#[derive(Debug, Parser)]
struct Cli {
    #[arg(long, value_name = "ID")]
    provider: String,

    #[arg(long, value_name = "ID")]
    model: String,

    #[arg(long = "option", value_name = "ID=VALUE")]
    options: Vec<String>,

    #[arg(long = "agent-profile", value_name = "ID")]
    agent_profile: String,

    #[arg(long, value_name = "TEXT")]
    message: String,

    #[arg(long = "provider-config", value_name = "PATH")]
    provider_config: Option<PathBuf>,
}

fn main() -> Result<()> {
    if let Some(exit_code) = kraai_runtime::run_internal_process() {
        std::process::exit(exit_code);
    }
    color_eyre::install()?;
    let cli = Cli::parse();
    let mut builder = RuntimeBuilder::new(kraai_providers::DefaultProviders)
        .use_current_executable_as_nushell_host();
    if let Some(path) = cli.provider_config {
        builder = builder.provider_config_path(path);
    }
    let runtime = builder.build();
    let executor = tokio::runtime::Runtime::new()?;
    let outcome = executor.block_on(kraai_headless::run(
        &runtime,
        RunOptions {
            provider_id: cli.provider,
            model_id: cli.model,
            options: cli.options,
            agent_profile: cli.agent_profile,
            message: cli.message,
        },
        &mut std::io::stdout().lock(),
    ));
    let shutdown = executor.block_on(runtime.shutdown());
    let RunOutcome { metrics, failure } = outcome?;
    if let Some(path) = std::env::var_os("KRAAI_EVAL_METRICS_PATH") {
        std::fs::write(path, serde_json::to_vec_pretty(&metrics)?)?;
    }
    shutdown?;
    match failure {
        Some(failure) => Err(eyre!(failure)),
        None => Ok(()),
    }
}
