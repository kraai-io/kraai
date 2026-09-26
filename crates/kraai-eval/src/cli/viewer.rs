use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use color_eyre::eyre::{Result, ensure};

#[derive(Debug, Args)]
pub(super) struct ViewerArgs {
    #[arg(long, default_value = ".kraai-eval-cache")]
    cache_dir: PathBuf,
    #[arg(
        long,
        default_value_t = 0,
        help = "Local port; 0 selects an available port"
    )]
    port: u16,
    #[arg(long, help = "Print the URL without opening a browser")]
    no_open: bool,
}

pub(super) fn execute(args: ViewerArgs, json: bool) -> Result<ExitCode> {
    ensure!(!json, "view does not support --json");
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(kraai_eval::viewer::serve(
            args.cache_dir,
            args.port,
            !args.no_open,
        ))?;
    Ok(ExitCode::SUCCESS)
}
