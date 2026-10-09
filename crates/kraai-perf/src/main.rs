#![forbid(unsafe_code)]

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use color_eyre::eyre::{Context, Result, ensure};

mod benchmark;
mod fingerprint;
mod fixtures;
mod measurement;
mod metadata;
mod process;
mod profiling;
mod report;
mod runner;
#[cfg(all(test, unix))]
mod test_support;
mod workloads;

#[derive(Parser)]
#[command(about = "Measure fixed offline Kraai workloads and compare saved results")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "List fixed workloads as JSON")]
    List,
    #[command(
        about = "Measure benchmark run phases and print JSON; setup and verification are excluded from timing"
    )]
    Run {
        #[arg(long, help = "Also save the JSON report to this file")]
        output: Option<PathBuf>,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..))]
        samples: u32,
        #[arg(long, default_value_t = 1)]
        warmups: u32,
        #[arg(long, help = "Run only this workload; repeat to select several")]
        workload: Vec<String>,
        #[arg(
            long,
            default_value = "target/kraai-perf-work",
            help = "Filesystem used for persistence workloads; keep identical when comparing"
        )]
        work_dir: PathBuf,
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..))]
        timeout_seconds: u32,
    },
    #[command(about = "Print JSON deltas; positive values mean higher resource use")]
    Compare {
        baseline: PathBuf,
        candidate: PathBuf,
        #[arg(
            long,
            help = "Flag median wall time, total CPU time, or peak RSS increases above this percent without changing the exit status"
        )]
        max_regression_percent: Option<f64>,
    },
    #[command(about = "Capture CPU flame graphs or allocation profiles for a fixed workload")]
    Profile(profiling::Args),
    #[command(hide = true)]
    ProfileWorker { workload: String },
    #[command(hide = true)]
    Sample { workload: String },
}

fn main() -> Result<()> {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == kraai_nushell_runtime::INTERNAL_HOST_ARGUMENT)
    {
        std::process::exit(kraai_nushell_runtime::run_host_process());
    }
    color_eyre::install()?;
    match Cli::parse().command {
        Command::List => print_json(&workloads::catalog()?)?,
        Command::Run {
            output,
            samples,
            warmups,
            workload,
            work_dir,
            timeout_seconds,
        } => {
            ensure!(
                env!("KRAAI_PERF_PROFILE") == "release",
                "performance runs require a release build; use `just perf run`"
            );
            if let Some(output) = &output {
                ensure!(
                    !output.exists(),
                    "output already exists: {}",
                    output.display()
                );
            }
            let selected = select_workloads(&workload)?;
            fs::create_dir_all(&work_dir)?;
            let work_dir = fs::canonicalize(work_dir)?;
            let metadata = metadata::collect(&work_dir, warmups)?;
            let measured = runner::run(&selected, samples, warmups, &work_dir, timeout_seconds)?;
            let report = report::Report::new(metadata, measured)?;
            if let Some(output) = &output {
                save_report(output, &report)?;
            }
            print_json(&report)?;
        }
        Command::Compare {
            baseline,
            candidate,
            max_regression_percent,
        } => {
            let baseline = read_report(&baseline)?;
            let candidate = read_report(&candidate)?;
            let comparison = report::compare(&baseline, &candidate, max_regression_percent)?;
            print_json(&comparison)?;
        }
        Command::Sample { workload } => {
            let sample = workloads::find(&workload)?.sample(false)?;
            print_json(&sample)?;
        }
        Command::Profile(arguments) => profiling::run(arguments)?,
        Command::ProfileWorker { workload } => {
            workloads::find(&workload)?.sample(true)?;
        }
    }
    Ok(())
}

fn select_workloads(names: &[String]) -> Result<Vec<benchmark::Workload>> {
    let catalog = workloads::catalog()?;
    for name in names {
        ensure!(
            catalog.iter().any(|workload| workload.name == *name),
            "unknown workload: {name}"
        );
    }
    Ok(catalog
        .into_iter()
        .filter(|workload| names.is_empty() || names.contains(&workload.name))
        .collect())
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer_pretty(&mut output, value)?;
    std::io::Write::write_all(&mut output, b"\n")?;
    Ok(())
}

fn read_report(path: &Path) -> Result<report::Report> {
    serde_json::from_reader(File::open(path)?)
        .wrap_err_with(|| format!("read performance report {}", path.display()))
}

fn save_report(path: &Path, report: &report::Report) -> Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, report)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)?;
    Ok(())
}
