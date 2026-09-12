mod display;
mod tasks;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use color_eyre::eyre::{Result, bail};
use kraai_eval::{
    HarnessProfile, KraaiProviderConfigRequest, ProgressReporter, RunRequest, RunResult, RunStatus,
    SuiteRequest, SuiteResult,
};
use serde::Serialize;

use display::{ProgressDisplay, format_comparison, format_result_summary, format_suite_summary};

#[derive(Debug, Parser)]
#[command(
    about = "Run and compare agent evaluations with executable hidden graders",
    after_help = "Examples:\n  kraai-eval list\n  kraai-eval show event-stream\n  kraai-eval check\n  kraai-eval run --model MODEL --attempts 3\n  kraai-eval run TASK --model MODEL --harness agent.toml\n  kraai-eval compare LEFT/summary.json RIGHT/summary.json"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "Task catalog directory; defaults to evals/tasks or bundled tasks"
    )]
    tasks_dir: Option<PathBuf>,
    #[arg(long, global = true, help = "Print machine-readable JSON to stdout")]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "List available tasks and their time budgets")]
    List,
    #[command(about = "Inspect a task's prompt and grading commands")]
    Show { task: String },
    #[command(
        about = "Verify that task baselines fail and reference solutions pass, without a model"
    )]
    Check { tasks: Vec<String> },
    #[command(about = "Run selected task IDs or manifest paths; defaults to every task")]
    Run(Box<RunArgs>),
    #[command(about = "Display a saved run result.json or suite summary.json")]
    Report { path: PathBuf },
    #[command(about = "Compare paired attempts with matching tasks, graders, models and budgets")]
    Compare { left: PathBuf, right: PathBuf },
}

#[derive(Debug, Args)]
struct RunArgs {
    tasks: Vec<String>,
    #[arg(long, help = "Model identifier, also recorded in result comparisons")]
    model: String,
    #[arg(
        long,
        help = "Harness TOML profile; defaults to Kraai with the subscription proxy",
        long_help = "Harness TOML profile with schema_version = 1, name, program, and args. Arguments support {model}, {prompt}, {workspace}, and {proxy_url}. Set proxy to none, openai, or codex_subscription. Kraai profiles can set sanitize_kraai_provider = true and use {provider_id} and {provider_config}. Relative executable paths resolve from the profile directory; bare names resolve on PATH. Nix-packaged executables include their runtime dependencies in the sandbox. See evals/harnesses/kraai.toml for a complete example."
    )]
    harness: Option<PathBuf>,
    #[arg(
        long,
        help = "Override the profile's executable, useful for a local build"
    )]
    runner: Option<PathBuf>,
    #[arg(
        long,
        help = "Kraai subscription provider ID; inferred when there is only one"
    )]
    provider: Option<String>,
    #[arg(
        long,
        help = "Kraai providers.toml path; defaults to the normal agent state directory"
    )]
    provider_config: Option<PathBuf>,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..))]
    attempts: u64,
    #[arg(long, default_value_t = 0)]
    start_attempt: u64,
    #[arg(long, default_value = ".kraai-eval-cache")]
    cache_dir: PathBuf,
    #[arg(long, help = "Reuse completed attempts with identical inputs")]
    resume: bool,
    #[arg(
        long,
        help = "Print resolved tasks and harness configuration without calling a model"
    )]
    dry_run: bool,
}

pub(super) fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let directory = cli.tasks_dir.unwrap_or_else(tasks::default_directory);
    match cli.command {
        Command::List => {
            let tasks = tasks::discover(&directory)?;
            if cli.json {
                print_json(&tasks)?;
            } else {
                for (id, task) in tasks {
                    println!(
                        "{id:<28} {:>4}s  {}",
                        task.manifest.runner.timeout_seconds,
                        task.path.display()
                    );
                }
            }
        }
        Command::Show { task } => {
            for task in tasks::select(&directory, &[task])? {
                if cli.json {
                    print_json(&task)?;
                } else {
                    println!(
                        "{}\n\n{}\n\nRunner budget: {}s\nManifest: {}",
                        task.manifest.id,
                        task.manifest.prompt.trim(),
                        task.manifest.runner.timeout_seconds,
                        task.path.display()
                    );
                    for command in task.manifest.grader.commands {
                        println!(
                            "Grader: {} [{}s]",
                            command.command.join(" "),
                            command.timeout_seconds
                        );
                    }
                }
            }
        }
        Command::Check { tasks: selectors } => {
            let tasks = tasks::select(&directory, &selectors)?;
            let mut results = Vec::new();
            for task in tasks {
                results.push(kraai_eval::validate_task(&task.path)?);
            }
            let passed = results.iter().all(kraai_eval::TaskValidation::passed);
            if cli.json {
                print_json(&results)?;
            } else {
                for result in results {
                    println!(
                        "{}: {}",
                        result.task_id,
                        if result.passed() {
                            "grader checks passed"
                        } else {
                            "GRADER CHECKS FAILED"
                        }
                    );
                    for diagnostic in result.diagnostics {
                        println!("{diagnostic}");
                    }
                }
            }
            return Ok(if passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
        Command::Run(args) => return execute(*args, &directory, cli.json),
        Command::Report { path } => return report(&path, cli.json),
        Command::Compare { left, right } => {
            let comparison = kraai_eval::compare(&left, &right)?;
            if cli.json {
                print_json(&comparison)?;
            } else {
                println!("{}", format_comparison(&comparison));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn execute(args: RunArgs, directory: &Path, json: bool) -> Result<ExitCode> {
    let tasks = tasks::select(directory, &args.tasks)?;
    let profile = args
        .harness
        .as_deref()
        .map(HarnessProfile::load)
        .transpose()?
        .unwrap_or_else(HarnessProfile::kraai);
    if !profile.sanitize_kraai_provider
        && (args.provider.is_some() || args.provider_config.is_some())
    {
        bail!(
            "--provider and --provider-config require a harness profile with sanitize_kraai_provider = true"
        );
    }
    let harness = profile.resolve(&args.model, args.runner.as_deref())?;
    let end_attempt = args
        .start_attempt
        .checked_add(args.attempts)
        .ok_or_else(|| color_eyre::eyre::eyre!("attempt range overflowed"))?;
    for task in &tasks {
        let mut manifest = task.manifest.clone();
        manifest.resolve_source_revision(task.path.parent().unwrap_or_else(|| Path::new(".")))?;
    }
    if args.dry_run {
        print_json(&serde_json::json!({
            "tasks": tasks.iter().map(|task| &task.manifest.id).collect::<Vec<_>>(),
            "harness": harness.name,
            "program": harness.program,
            "args": harness.args,
            "version": harness.version,
            "model": harness.model_label,
            "proxy": profile.proxy,
            "max_requests": profile.max_requests,
            "attempts": args.attempts,
            "start_attempt": args.start_attempt,
            "resume": args.resume,
            "cache_dir": args.cache_dir,
        }))?;
        return Ok(ExitCode::SUCCESS);
    }
    let provider_config = if profile.sanitize_kraai_provider {
        Some(KraaiProviderConfigRequest::new(
            args.provider_config.map_or_else(
                || kraai_persistence::agent_state_root().map(|root| root.join("providers.toml")),
                Ok,
            )?,
            args.provider,
        ))
    } else {
        None
    };
    fs::create_dir_all(&args.cache_dir)?;
    let output_root = args.cache_dir.canonicalize()?;
    let progress = ProgressReporter::new();
    let display = ProgressDisplay::start(progress.clone());
    let mut runs = Vec::new();
    for task in tasks {
        for attempt in args.start_attempt..end_attempt {
            runs.push(RunRequest {
                task_path: task.path.clone(),
                runner_program: harness.program.clone(),
                runner_args: harness.args.clone(),
                runner_version: harness.version.clone(),
                harness_name: Some(harness.name.clone()),
                model_label: Some(harness.model_label.clone()),
                attempt,
                cache_dir: output_root.clone(),
                reuse_result: args.resume,
                model_proxy: profile.model_proxy(),
                kraai_provider_config: provider_config.clone(),
                progress: Some(progress.clone()),
            });
        }
    }
    let result = kraai_eval::run_suite(&SuiteRequest {
        runs,
        output_root: output_root.clone(),
    });
    display.finish();
    let result = result?;
    if json {
        print_json(&result)?;
    } else {
        println!("{}", format_suite_summary(&result, &output_root));
        for run in &result.runs {
            println!(
                "  {} attempt {}: {}",
                run.task_id.as_deref().unwrap_or("unknown task"),
                run.attempt,
                run.error
                    .as_deref()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!(
                        "{:?}",
                        run.status.as_ref().unwrap_or(&RunStatus::ControllerFailed)
                    ))
            );
        }
        println!(
            "Report: {}",
            output_root
                .join(&result.artifact_path)
                .join("summary.json")
                .display()
        );
    }
    Ok(suite_exit_code(&result))
}

fn report(path: &Path, json: bool) -> Result<ExitCode> {
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    if value.get("suite_id").is_some() {
        let result: SuiteResult = serde_json::from_value(value)?;
        if json {
            print_json(&result)?;
        } else {
            println!(
                "{}",
                format_suite_summary(&result, &report_cache_root(path, &result.artifact_path)?)
            );
        }
        Ok(suite_exit_code(&result))
    } else {
        let result: RunResult = serde_json::from_value(value)?;
        if json {
            print_json(&result)?;
        } else {
            println!(
                "{}",
                format_result_summary(&result, &report_cache_root(path, &result.artifact_path)?)
            );
        }
        Ok(if result.status == RunStatus::Passed {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        })
    }
}

fn report_cache_root(path: &Path, artifact_path: &Path) -> Result<PathBuf> {
    let canonical = path.canonicalize()?;
    let mut root = canonical
        .parent()
        .ok_or_else(|| color_eyre::eyre::eyre!("report path has no parent"))?;
    if !root.ends_with(artifact_path) || artifact_path.is_absolute() {
        bail!("report must remain inside its recorded artifact directory");
    }
    for _ in artifact_path.components() {
        root = root
            .parent()
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid report artifact path"))?;
    }
    Ok(root.to_path_buf())
}

fn suite_exit_code(result: &SuiteResult) -> ExitCode {
    if result.passed_runs == result.requested_runs {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_requires_a_model_and_rejects_zero_attempts() {
        assert!(Cli::try_parse_from(["kraai-eval", "run"]).is_err());
        assert!(
            Cli::try_parse_from(["kraai-eval", "run", "--model", "test", "--attempts", "0"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "kraai-eval",
                "run",
                "task",
                "--model",
                "test",
                "--resume",
                "--json"
            ])
            .is_ok()
        );
    }
}
