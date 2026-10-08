#![forbid(unsafe_code)]

use std::path::PathBuf;

use clap::Parser;
use color_eyre::eyre::{Result, eyre};
use kraai_runtime::{RuntimeBuilder, RuntimeStartupState};

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[arg(long)]
    provider: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long = "option", value_name = "ID=VALUE")]
    options: Vec<String>,
    #[arg(long)]
    agent_profile: Option<String>,
    #[arg(long)]
    provider_config: Option<PathBuf>,
    #[arg(long)]
    storage_root: Option<PathBuf>,
}

fn main() -> Result<()> {
    if let Some(code) = kraai_runtime::run_internal_process() {
        std::process::exit(code);
    }
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    tokio::runtime::Runtime::new()?.block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    let mut builder = RuntimeBuilder::new().use_current_executable_as_nushell_host();
    if let Some(path) = cli.provider_config {
        builder = builder.provider_config_path(path);
    }
    if let Some(path) = cli.storage_root {
        builder = builder.storage_root(path);
    }
    let runtime = builder.build_on(&tokio::runtime::Handle::current());
    let result = async {
        if let RuntimeStartupState::Failed(error) = runtime.wait_for_startup().await? {
            return Err(eyre!(error));
        }
        let options = kraai_acp::Options { provider: cli.provider, model: cli.model, profile: cli.agent_profile, options: cli.options };
        tokio::select! {
            result = kraai_acp::serve(runtime.clone(), options, kraai_acp::Stdio::new()) => result.map_err(|error| eyre!(error)),
            result = tokio::signal::ctrl_c() => result.map_err(Into::into),
        }
    }.await;
    let shutdown = runtime.shutdown().await;
    result?;
    shutdown?;
    Ok(())
}
