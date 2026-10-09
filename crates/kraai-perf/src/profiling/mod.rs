use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::ValueEnum;
use color_eyre::eyre::{Result, WrapErr, ensure};
use serde::Serialize;

use crate::{metadata, process, workloads};

mod allocations;
mod artifacts;
mod cpu;

#[derive(clap::Args)]
pub struct Args {
    #[arg(value_enum)]
    kind: Kind,
    #[arg(long)]
    workload: String,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..))]
    samples: u32,
    #[arg(long, default_value_t = 1)]
    warmups: u32,
    #[arg(
        long,
        help = "New artifact directory; defaults to a unique directory under target/perf"
    )]
    output: Option<PathBuf>,
    #[arg(long, default_value = "target/kraai-perf-work")]
    work_dir: PathBuf,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u32).range(1..))]
    timeout_seconds: u32,
}

#[derive(Clone, Copy, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Cpu,
    Allocations,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Allocations => "allocations",
        }
    }

    fn tools(self) -> &'static [&'static str] {
        match self {
            Self::Cpu => &["perf", "inferno-collapse-perf", "inferno-flamegraph"],
            Self::Allocations => &["valgrind", "inferno-flamegraph"],
        }
    }
}

#[derive(Serialize)]
struct Capture {
    sample: u32,
    directory: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    allocations: Vec<allocations::AllocationSummary>,
}

#[derive(Serialize)]
struct ToolIdentity {
    executable: PathBuf,
    sha256: String,
    version: Option<String>,
}

pub fn run(arguments: Args) -> Result<()> {
    ensure!(
        cfg!(target_os = "linux"),
        "CPU and allocation profiling currently require Linux"
    );
    ensure!(
        env!("KRAAI_PERF_PROFILE") == "release" && env!("KRAAI_PERF_DEBUG") == "true",
        "profiling requires an optimized build with debug symbols; use `just profile`"
    );
    let workload = workloads::find(&arguments.workload)?;
    let output = match arguments.output {
        Some(path) => path,
        None => PathBuf::from("target/perf").join(format!(
            "{}-{}-{}",
            workload.name,
            arguments.kind.name(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        )),
    };
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    ensure!(
        !output.exists(),
        "output already exists: {}",
        output.display()
    );
    let staging = tempfile::Builder::new()
        .prefix(".kraai-profile-")
        .tempdir_in(parent)?;
    let directory = fs::canonicalize(staging.path())?;
    fs::create_dir_all(&arguments.work_dir)?;
    let work_dir = fs::canonicalize(&arguments.work_dir)?;
    let interrupted = process::Interrupt::new()?;
    let context = Context {
        directory: &directory,
        timeout: Duration::from_secs(u64::from(arguments.timeout_seconds)),
        interrupted: &interrupted.flag,
    };
    let executable = std::env::current_exe()?;
    let tools = context.versions(arguments.kind.tools(), &work_dir)?;
    let metadata = metadata::collect(&work_dir, arguments.warmups)?;
    for index in 0..arguments.warmups {
        let workspace = process::IsolatedWorkspace::new(&work_dir)?;
        let mut command = Command::new(&executable);
        command.args(["profile-worker", &workload.name]);
        workspace.configure(&mut command);
        context.execute(&format!("warmup-{index}"), &mut command, None)?;
    }
    let mut folded = File::create(directory.join("stacks.folded"))?;
    let mut perf_script = File::create(directory.join("stacks.perf"))?;
    let mut captures = Vec::new();
    for sample in 1..=arguments.samples {
        let relative = format!("captures/{sample:04}");
        let capture = directory.join(&relative);
        fs::create_dir_all(&capture)?;
        let workspace = process::IsolatedWorkspace::new(&work_dir)?;
        let mut command = match arguments.kind {
            Kind::Cpu => {
                cpu::record_command(&capture.join("perf.data"), &executable, &workload.name)
            }
            Kind::Allocations => allocations::record_command(&capture, &executable, &workload.name),
        };
        workspace.configure(&mut command);
        context.execute(&format!("{relative}/record"), &mut command, None)?;
        let allocations = match arguments.kind {
            Kind::Cpu => {
                let decoded = capture.join("perf.script");
                let mut command = cpu::decode_command(&capture.join("perf.data"));
                workspace.configure(&mut command);
                context.execute(&format!("{relative}/decode"), &mut command, Some(&decoded))?;
                std::io::copy(&mut File::open(&decoded)?, &mut perf_script)?;
                perf_script.write_all(b"\n")?;
                Vec::new()
            }
            Kind::Allocations => allocations::summarize(&capture, &mut folded)?,
        };
        captures.push(Capture {
            sample,
            directory: relative,
            allocations,
        });
    }
    drop(folded);
    drop(perf_script);
    let render_workspace = process::IsolatedWorkspace::new(&work_dir)?;
    if matches!(arguments.kind, Kind::Cpu) {
        let mut command = cpu::collapse_command(&directory.join("stacks.perf"));
        render_workspace.configure(&mut command);
        context.execute(
            "collapse",
            &mut command,
            Some(&directory.join("stacks.folded")),
        )?;
    } else {
        fs::remove_file(directory.join("stacks.perf"))?;
    }
    ensure!(
        fs::metadata(directory.join("stacks.folded"))?.len() > 0,
        "profile contains no stacks; increase --samples for short CPU workloads"
    );
    let flamegraph = format!("{}.svg", arguments.kind.name());
    let title = format!(
        "{}: {}",
        workload.name,
        match arguments.kind {
            Kind::Cpu => "user-space CPU samples",
            Kind::Allocations => "total allocated bytes",
        }
    );
    let mut command = artifacts::render_command(
        &directory.join("stacks.folded"),
        &title,
        match arguments.kind {
            Kind::Cpu => "samples",
            Kind::Allocations => "bytes",
        },
    );
    render_workspace.configure(&mut command);
    context.execute("render", &mut command, Some(&directory.join(&flamegraph)))?;
    artifacts::validate_svg(&directory.join(&flamegraph))?;
    let report = serde_json::json!({
        "schema_version": 2,
        "kind": arguments.kind,
        "workload": workload,
        "samples": arguments.samples,
        "metadata": metadata,
        "tools": tools,
        "scope": "Full worker and child process lifetimes, including fixture setup, benchmark run, verification, and cleanup. Only uninstrumented benchmark timings isolate the run phase; profiles cover the full lifecycle.",
        "nushell_profiling_timeouts_seconds": {"startup": 60, "execution": 60},
        "configuration": match arguments.kind {
            Kind::Cpu => serde_json::json!({"event": cpu::EVENT, "frequency_hz": cpu::FREQUENCY_HZ, "call_graph": cpu::CALL_GRAPH, "flamegraph_weight": "samples", "includes_kernel_cpu": false, "includes_blocked_time": false}),
            Kind::Allocations => serde_json::json!({"tool": "dhat", "trace_children": true, "stack_depth": 64, "flamegraph_weight": "total_allocated_bytes", "peak_scope": "per-process live heap; not RSS or simultaneous process-tree memory"}),
        },
        "captures": captures,
        "flamegraph": flamegraph,
        "folded_stacks": "stacks.folded",
    });
    let mut file = File::create(directory.join("profile.json"))?;
    serde_json::to_writer_pretty(&mut file, &report)?;
    file.sync_all()?;
    artifacts::publish(staging, &output)?;
    crate::print_json(&serde_json::json!({
        "output": fs::canonicalize(&output)?,
        "flamegraph": output.join(flamegraph),
        "report": output.join("profile.json"),
    }))
}

struct Context<'a> {
    directory: &'a Path,
    timeout: Duration,
    interrupted: &'a AtomicBool,
}

impl Context<'_> {
    fn execute(&self, label: &str, command: &mut Command, stdout: Option<&Path>) -> Result<()> {
        command.env("DEBUGINFOD_URLS", "");
        let default_stdout = self.directory.join(format!("{label}.stdout"));
        process::run(
            command,
            self.timeout,
            self.interrupted,
            stdout.unwrap_or(&default_stdout),
            &self.directory.join(format!("{label}.stderr")),
        )
        .wrap_err_with(|| format!("profiling stage {label} failed"))
    }

    fn versions(&self, tools: &[&str], work_dir: &Path) -> Result<BTreeMap<String, ToolIdentity>> {
        let workspace = process::IsolatedWorkspace::new(work_dir)?;
        let mut versions = BTreeMap::new();
        for tool in tools {
            let mut command = Command::new(tool);
            let has_version = !tool.starts_with("inferno-");
            command.arg(if has_version { "--version" } else { "--help" });
            workspace.configure(&mut command);
            let label = format!("version-{tool}");
            self.execute(&label, &mut command, None)?;
            let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|directory| directory.join(tool))
                .find(|path| path.exists())
                .ok_or_else(|| color_eyre::eyre::eyre!("could not locate profiling tool {tool}"))?;
            let executable = fs::canonicalize(executable)?;
            versions.insert(
                (*tool).into(),
                ToolIdentity {
                    sha256: crate::fingerprint::file(&executable)?,
                    executable,
                    version: if has_version {
                        Some(
                            fs::read_to_string(self.directory.join(format!("{label}.stdout")))?
                                .trim()
                                .into(),
                        )
                    } else {
                        None
                    },
                },
            );
        }
        Ok(versions)
    }
}
