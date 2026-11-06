mod display;
mod in_house;
mod pricing;
mod proxy;
mod tasks;
mod terminal_bench;
mod viewer;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use color_eyre::eyre::{Result, ensure};
use kraai_eval::{EvalRoot, HarnessDir, HarnessProfile, ResolvedHarness};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    about = "Run agent benchmarks and keep every attempt under ~/.kraai/eval",
    after_help = "Examples:\n  kraai-eval run in-house --model MODEL --reasoning high --tasks 3 --attempts 5\n  kraai-eval run terminal-bench --model MODEL --tasks 20 --harness evals/harnesses/codex.toml\n  kraai-eval check in-house repository-repair\n  kraai-eval view\n\nRerunning the same command resumes it. Raising --tasks or --attempts only runs the new attempts."
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "Results directory; defaults to ~/.kraai/eval"
    )]
    eval_dir: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "In-house task directory; defaults to evals/tasks or bundled tasks"
    )]
    tasks_dir: Option<PathBuf>,
    #[arg(long, global = true, help = "Print machine-readable JSON to stdout")]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Run a benchmark, resuming any saved attempts for the same harness")]
    Run(Box<RunArgs>),
    #[command(
        about = "Verify tasks without a model: baselines must fail and reference solutions pass"
    )]
    Check(CheckArgs),
    #[command(about = "Open saved results in a local browser")]
    View(viewer::ViewerArgs),
    #[command(
        hide = true,
        about = "Run a credential proxy until stdin closes; writes private readiness and public metrics files"
    )]
    Proxy(Box<proxy::ProxyArgs>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Benchmark {
    InHouse,
    TerminalBench,
}

impl Benchmark {
    fn directory(self) -> &'static str {
        match self {
            Self::InHouse => "in-house",
            Self::TerminalBench => "terminal-bench-4.0.0",
        }
    }
}

#[derive(Debug, Args)]
struct RunArgs {
    benchmark: Benchmark,
    #[arg(long, help = "Model ID passed to the harness")]
    model: String,
    #[arg(
        long,
        help = "Reasoning effort, passed through the harness profile's reasoning_args"
    )]
    reasoning: Option<String>,
    #[arg(
        long,
        help = "Kraai provider ID from providers.toml; its type picks the proxy. Inferred when there is only one openai-codex or openai-chat-completions provider"
    )]
    provider: Option<String>,
    #[arg(
        long,
        help = "Kraai providers.toml path; defaults to the normal agent state directory"
    )]
    provider_config: Option<PathBuf>,
    #[arg(
        long,
        help = "Harness TOML profile; defaults to Kraai",
        long_help = "Harness TOML profile with schema_version = 1, name, program, and args. Arguments support {model}, {prompt}, {workspace}, and {proxy_url}. reasoning_args use {reasoning} and go before args when --reasoning is set. Set proxy to none, openai, or codex_subscription. Kraai profiles set sanitize_kraai_provider = true instead, use {provider_id} and {provider_config}, and get the proxy that matches the selected provider's type. Relative executable paths resolve from the profile directory; bare names resolve on PATH. See evals/harnesses for examples."
    )]
    harness: Option<PathBuf>,
    #[arg(
        long,
        help = "Override the profile's executable, useful for a local build"
    )]
    runner: Option<PathBuf>,
    #[arg(
        long,
        value_parser = clap::value_parser!(u64).range(1..),
        help = "Number of tasks, taken in a fixed order; in-house defaults to every task, terminal-bench requires it"
    )]
    tasks: Option<u64>,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..))]
    attempts: u64,
    #[arg(
        long,
        help = "Terminal-Bench only: controller IP reachable from benchmark containers; defaults to the local Docker bridge gateway"
    )]
    proxy_host: Option<String>,
    #[command(flatten)]
    pricing: pricing::PricingArgs,
    #[arg(long, help = "Print the resolved plan without calling a model")]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct CheckArgs {
    benchmark: Benchmark,
    #[arg(help = "Task names to check; defaults to every task")]
    tasks: Vec<String>,
}

struct Prepared {
    root: EvalRoot,
    profile: HarnessProfile,
    harness: ResolvedHarness,
    directory: HarnessDir,
}

pub(super) fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let eval_dir = cli.eval_dir.map_or_else(EvalRoot::default_path, Ok)?;
    let tasks_dir = cli.tasks_dir.unwrap_or_else(tasks::default_directory);
    match cli.command {
        Command::Run(args) => {
            let prepared = prepare(&args, &eval_dir)?;
            match args.benchmark {
                Benchmark::InHouse => in_house::run(*args, prepared, &tasks_dir, cli.json),
                Benchmark::TerminalBench => terminal_bench::run(*args, prepared, cli.json),
            }
        }
        Command::Check(args) => match args.benchmark {
            Benchmark::InHouse => in_house::check(&tasks_dir, &args.tasks, cli.json),
            Benchmark::TerminalBench => {
                terminal_bench::check(&EvalRoot::open(&eval_dir)?, &args.tasks)
            }
        },
        Command::View(args) => viewer::execute(args, eval_dir, cli.json),
        Command::Proxy(args) => proxy::execute(*args),
    }
}

fn prepare(args: &RunArgs, eval_dir: &std::path::Path) -> Result<Prepared> {
    let profile = args
        .harness
        .as_deref()
        .map(HarnessProfile::load)
        .transpose()?
        .unwrap_or_else(HarnessProfile::kraai);
    ensure!(
        profile.sanitize_kraai_provider
            || (args.provider.is_none() && args.provider_config.is_none()),
        "--provider and --provider-config require a harness profile with sanitize_kraai_provider = true"
    );
    ensure!(
        args.benchmark == Benchmark::TerminalBench || args.proxy_host.is_none(),
        "--proxy-host only applies to terminal-bench"
    );
    let harness = profile.resolve(
        &args.model,
        args.reasoning.as_deref(),
        args.runner.as_deref(),
    )?;
    let root = EvalRoot::open(eval_dir)?;
    let directory = root.harness_dir(
        args.benchmark.directory(),
        &kraai_eval::model_dir(&harness.model, harness.reasoning.as_deref()),
        &harness.name,
        &harness.version,
    )?;
    Ok(Prepared {
        root,
        profile,
        harness,
        directory,
    })
}

fn provider_config_path(args: &RunArgs) -> Result<PathBuf> {
    args.provider_config.clone().map_or_else(
        || kraai_persistence::agent_state_root().map(|root| root.join("providers.toml")),
        Ok,
    )
}

fn lock(root: &EvalRoot, directory: &HarnessDir) -> Result<kraai_io::lock::FileLock> {
    let path = root.lock_path(directory);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    kraai_io::lock::FileLock::try_acquire(file).map_err(|error| {
        color_eyre::eyre::eyre!("{} is already running: {error}", directory.path.display())
    })
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn exit_code(success: bool) -> ExitCode {
    if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_prices_need_input_and_output() -> Result<()> {
        let run = |extra: &[&str]| {
            let mut args = vec!["kraai-eval", "run", "in-house", "--model", "test"];
            args.extend_from_slice(extra);
            Cli::try_parse_from(args)
        };
        let Command::Run(args) = run(&[
            "--price-input",
            "1.25",
            "--price-output",
            "10",
            "--price-cache-read",
            "0.125",
        ])?
        .command
        else {
            color_eyre::eyre::bail!("expected the run command");
        };
        let rates = args
            .pricing
            .options()?
            .rates
            .ok_or_else(|| color_eyre::eyre::eyre!("missing flat rates"))?;
        ensure!(rates.input == kraai_types::Usd(1_250_000_000));
        ensure!(rates.cache_read == Some(kraai_types::Usd(125_000_000)));
        ensure!(rates.reasoning.is_none());
        ensure!(run(&["--price-input", "1"]).is_err());
        ensure!(run(&["--price-output", "1"]).is_err());
        Ok(())
    }

    #[test]
    fn run_requires_a_known_benchmark_a_model_and_positive_counts() {
        assert!(Cli::try_parse_from(["kraai-eval", "run", "in-house"]).is_err());
        assert!(Cli::try_parse_from(["kraai-eval", "run", "swe-bench", "--model", "m"]).is_err());
        for zero in ["--attempts", "--tasks"] {
            assert!(
                Cli::try_parse_from(["kraai-eval", "run", "in-house", "--model", "m", zero, "0"])
                    .is_err()
            );
        }
        assert!(
            Cli::try_parse_from([
                "kraai-eval",
                "run",
                "terminal-bench",
                "--model",
                "m",
                "--reasoning",
                "high",
                "--tasks",
                "3",
                "--attempts",
                "5",
                "--json",
            ])
            .is_ok()
        );
    }
}
