#![forbid(unsafe_code)]

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use color_eyre::eyre::Result;
use kraai_eval::KraaiProviderConfigRequest;
use kraai_eval::ModelProxyRequest;
use kraai_eval::ProgressReporter;
use kraai_eval::RunRequest;
use kraai_eval::RunResult;
use kraai_eval::RunStatus;
use kraai_eval::SuiteRequest;
use kraai_eval::SuiteResult;

struct ProgressDisplay {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    interactive: bool,
}

impl ProgressDisplay {
    fn start(reporter: ProgressReporter) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let interactive = io::stderr().is_terminal();
        let thread = std::thread::spawn(move || {
            let started = Instant::now();
            let mut previous_phase = String::new();
            while !thread_stop.load(Ordering::Relaxed) {
                let snapshot = reporter.snapshot();
                if !snapshot.task_id.is_empty() && (interactive || snapshot.phase != previous_phase)
                {
                    let line = format_progress_line(started.elapsed(), &snapshot);
                    let mut stderr = io::stderr().lock();
                    if interactive {
                        let _ = write!(stderr, "\r\x1b[2K{line}");
                        let _ = stderr.flush();
                    } else {
                        let _ = writeln!(stderr, "{line}");
                    }
                    previous_phase = snapshot.phase;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        Self {
            stop,
            thread: Some(thread),
            interactive,
        }
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if self.interactive {
            let mut stderr = io::stderr().lock();
            let _ = write!(stderr, "\r\x1b[2K");
            let _ = stderr.flush();
        }
    }
}

impl Drop for ProgressDisplay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn format_progress_line(elapsed: Duration, snapshot: &kraai_eval::ProgressSnapshot) -> String {
    let seconds = elapsed.as_secs();
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    let model = snapshot.model_label.as_deref().unwrap_or("unlabeled-model");
    format!(
        "[{hours:02}:{minutes:02}:{seconds:02}] task={} | harness={}@{} | model={} | attempt={} | {}",
        snapshot.task_id,
        snapshot.harness_name,
        snapshot.runner_version,
        model,
        snapshot.attempt,
        snapshot.phase
    )
}

fn format_result_summary(result: &RunResult, cache_dir: &std::path::Path) -> String {
    let status = match result.status {
        RunStatus::Passed => "PASSED",
        RunStatus::Failed => "FAILED",
        RunStatus::RunnerFailed => "RUNNER FAILED",
        RunStatus::ControllerFailed => "CONTROLLER FAILED",
    };
    let model = result.model_label.as_deref().unwrap_or("unlabeled-model");
    let runner = result
        .runner
        .as_ref()
        .map(process_outcome)
        .unwrap_or("not started");
    let graders = if result.graders.is_empty()
        && matches!(
            result.status,
            RunStatus::RunnerFailed | RunStatus::ControllerFailed
        ) {
        String::from("skipped")
    } else {
        let passed = result
            .graders
            .iter()
            .filter(|grader| process_succeeded(grader))
            .count();
        format!("{passed}/{} passed", result.graders.len())
    };
    let artifacts = cache_dir.join(&result.artifact_path);
    let runner_duration = result.runner.as_ref().map_or_else(
        || String::from("n/a"),
        |runner| format_duration_ms(runner.duration_ms),
    );
    let usage = result.metrics.usage().map_or_else(
        || String::from("unavailable"),
        |usage| {
            format!(
                "{} total ({} input, {} output, {} reasoning, {} cache read)",
                usage.total_tokens,
                usage.input_tokens,
                usage.output_tokens,
                usage.reasoning_tokens,
                usage.cache_read_tokens
            )
        },
    );
    let failure = result
        .controller_failure
        .as_ref()
        .map_or_else(String::new, |failure| {
            format!("\nController failure: {}: {}", failure.phase, failure.error)
        });
    format!(
        "Result: {status}\nTask: {}\nHarness: {}\nVersion: {}\nModel: {}\nAttempt: {}\nElapsed: {}\nRunner: {} ({})\nGraders: {}\nTokens: {}{}\nArtifacts: {}",
        result.task_id,
        result.harness_name,
        result.runner_version,
        model,
        result.attempt,
        format_duration_ms(result.duration_ms),
        runner,
        runner_duration,
        graders,
        usage,
        failure,
        artifacts.display()
    )
}

fn format_suite_summary(result: &SuiteResult, cache_dir: &std::path::Path) -> String {
    let success_rate = result.success_rate.map_or_else(
        || String::from("n/a"),
        |rate| format!("{:.1}%", rate * 100.0),
    );
    let token_mean = result
        .total_tokens
        .distribution
        .mean
        .map_or_else(|| String::from("n/a"), |mean| format!("{mean:.0}"));
    format!(
        "Suite complete\nRuns: {} requested, {} evaluated\nResults: {} passed, {} failed, {} controller failures, {} launch failures\nSuccess rate: {}\nElapsed: {}\nTokens: {} total, {} mean per measured run\nArtifacts: {}",
        result.requested_runs,
        result.evaluated_runs,
        result.passed_runs,
        result.failed_runs,
        result.controller_failures,
        result.launch_failures,
        success_rate,
        format_duration_ms(result.duration_ms),
        result.total_tokens.total,
        token_mean,
        cache_dir.join(&result.artifact_path).display(),
    )
}

fn process_succeeded(process: &kraai_eval::ProcessRecord) -> bool {
    process.exit_code == Some(0) && !process.timed_out && !process.output_limit_exceeded
}

fn process_outcome(process: &kraai_eval::ProcessRecord) -> &'static str {
    if process.timed_out {
        "timed out"
    } else if process.output_limit_exceeded {
        "output limit exceeded"
    } else if process.exit_code == Some(0) {
        "passed"
    } else {
        "failed"
    }
}

fn format_duration_ms(milliseconds: u128) -> String {
    if milliseconds < 1_000 {
        return format!("{milliseconds}ms");
    }
    let seconds = milliseconds / 1_000;
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        let tenths = (milliseconds % 1_000) / 100;
        format!("{seconds}.{tenths}s")
    }
}

#[derive(Debug, Parser)]
#[command(about = "Run reproducible, hidden-grader agent evaluations")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        task: PathBuf,
        #[command(flatten)]
        harness: HarnessArgs,
        #[arg(long, default_value_t = 0)]
        attempt: u64,
        #[arg(long, default_value = ".kraai-eval-cache")]
        cache_dir: PathBuf,
        #[arg(long)]
        reuse_result: bool,
        #[command(flatten)]
        proxy: ProxyArgs,
    },
    Suite {
        #[arg(long, required = true)]
        task: Vec<PathBuf>,
        #[command(flatten)]
        harness: HarnessArgs,
        #[arg(long, default_value_t = 3)]
        attempts: u64,
        #[arg(long, default_value_t = 0)]
        start_attempt: u64,
        #[arg(long, default_value = ".kraai-eval-cache")]
        cache_dir: PathBuf,
        #[command(flatten)]
        proxy: ProxyArgs,
    },
}

#[derive(Debug, Clone, Args)]
struct HarnessArgs {
    #[arg(long)]
    runner_program: PathBuf,
    #[arg(long = "runner-arg", allow_hyphen_values = true)]
    runner_args: Vec<String>,
    #[arg(long)]
    runner_version: String,
    #[arg(long)]
    harness_name: Option<String>,
    #[arg(long)]
    model_label: Option<String>,
}

#[derive(Debug, Clone, Args)]
struct ProxyArgs {
    #[arg(long, conflicts_with = "codex_subscription_proxy")]
    openai_proxy: bool,
    #[arg(long, default_value = "OPENAI_API_KEY", requires = "openai_proxy")]
    openai_api_key_env: String,
    #[arg(long, default_value_t = 64, requires = "openai_proxy")]
    openai_proxy_max_requests: u64,
    #[arg(long, conflicts_with = "openai_proxy")]
    codex_subscription_proxy: bool,
    #[arg(long, default_value_t = 64, requires = "codex_subscription_proxy")]
    codex_proxy_max_requests: u64,
    #[arg(long, requires = "codex_subscription_proxy")]
    kraai_provider_config: Option<PathBuf>,
    #[arg(long, requires = "codex_subscription_proxy")]
    kraai_provider_id: Option<String>,
}

impl ProxyArgs {
    fn resolve(
        &self,
    ) -> Result<(
        Option<ModelProxyRequest>,
        Option<KraaiProviderConfigRequest>,
    )> {
        let provider_config = if self.codex_subscription_proxy {
            Some(KraaiProviderConfigRequest::new(
                self.kraai_provider_config.clone().map_or_else(
                    || {
                        kraai_persistence::agent_state_root()
                            .map(|root| root.join("providers.toml"))
                    },
                    Ok,
                )?,
                self.kraai_provider_id.clone(),
            ))
        } else {
            None
        };
        let model_proxy = if self.codex_subscription_proxy {
            Some(ModelProxyRequest::codex_subscription(
                self.codex_proxy_max_requests,
            ))
        } else {
            self.openai_proxy.then(|| {
                ModelProxyRequest::openai(
                    self.openai_api_key_env.clone(),
                    self.openai_proxy_max_requests,
                )
            })
        };
        Ok((model_proxy, provider_config))
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;
    match Cli::parse().command {
        Command::Run {
            task,
            harness,
            attempt,
            cache_dir,
            reuse_result,
            proxy,
        } => {
            let result_cache_dir = cache_dir.clone();
            let (model_proxy, provider_config) = proxy.resolve()?;
            let progress = ProgressReporter::new();
            let display = ProgressDisplay::start(progress.clone());
            let result = kraai_eval::run(&RunRequest {
                task_path: task,
                runner_program: harness.runner_program,
                runner_args: harness.runner_args,
                runner_version: harness.runner_version,
                harness_name: harness.harness_name,
                model_label: harness.model_label,
                attempt,
                cache_dir,
                reuse_result,
                model_proxy,
                kraai_provider_config: provider_config,
                progress: Some(progress),
            });
            display.finish();
            let result = result?;
            println!("{}", format_result_summary(&result, &result_cache_dir));
        }
        Command::Suite {
            task,
            harness,
            attempts,
            start_attempt,
            cache_dir,
            proxy,
        } => {
            if attempts == 0 {
                color_eyre::eyre::bail!("suite attempts must be greater than zero");
            }
            std::fs::create_dir_all(&cache_dir)?;
            let output_root = cache_dir.canonicalize()?;
            let (model_proxy, provider_config) = proxy.resolve()?;
            let progress = ProgressReporter::new();
            let display = ProgressDisplay::start(progress.clone());
            let mut runs = Vec::new();
            let end_attempt = start_attempt
                .checked_add(attempts)
                .ok_or_else(|| color_eyre::eyre::eyre!("suite attempt range overflowed"))?;
            for task_path in task {
                for attempt in start_attempt..end_attempt {
                    runs.push(RunRequest {
                        task_path: task_path.clone(),
                        runner_program: harness.runner_program.clone(),
                        runner_args: harness.runner_args.clone(),
                        runner_version: harness.runner_version.clone(),
                        harness_name: harness.harness_name.clone(),
                        model_label: harness.model_label.clone(),
                        attempt,
                        cache_dir: output_root.clone(),
                        reuse_result: true,
                        model_proxy: model_proxy.clone(),
                        kraai_provider_config: provider_config.clone(),
                        progress: Some(progress.clone()),
                    });
                }
            }
            let result = kraai_eval::run_suite(&SuiteRequest { runs, output_root });
            display.finish();
            let result = result?;
            println!("{}", format_suite_summary(&result, &cache_dir));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_line_contains_elapsed_run_coordinates_and_phase() {
        let snapshot = kraai_eval::ProgressSnapshot {
            task_id: String::from("plural-files"),
            harness_name: String::from("kraai"),
            runner_version: String::from("git:abc123"),
            model_label: Some(String::from("gpt-test")),
            attempt: 2,
            phase: String::from("running grader 1/3"),
        };
        assert_eq!(
            format_progress_line(Duration::from_secs(3_661), &snapshot),
            "[01:01:01] task=plural-files | harness=kraai@git:abc123 | model=gpt-test | attempt=2 | running grader 1/3"
        );
    }

    #[test]
    fn result_summary_is_human_readable_and_points_to_artifacts() {
        let result = RunResult {
            schema_version: 6,
            experiment_id: String::from("deadbeef"),
            artifact_path: PathBuf::from("runs/task/kraai/git-abc/model/attempt-1/deadbeef"),
            task_id: String::from("task"),
            harness_name: String::from("kraai"),
            model_label: Some(String::from("model")),
            attempt: 1,
            runner_version: String::from("git:abc"),
            runner_artifact_sha256: String::from("runner-hash"),
            task_sha256: String::from("task-hash"),
            grader_sha256: String::from("grader-hash"),
            sandbox: kraai_eval::SandboxRecord {
                backend: String::from("bubblewrap+systemd-cgroup-v2"),
                network: kraai_eval::NetworkPolicy::Disabled,
                environment_cleared: true,
                max_memory_bytes: 1,
                max_processes: 1,
                cpu_quota_percent: 100,
            },
            status: RunStatus::Failed,
            runner: Some(kraai_eval::ProcessRecord {
                command: vec![String::from("kraai")],
                exit_code: Some(0),
                timed_out: false,
                output_limit_exceeded: false,
                duration_ms: 2_345,
            }),
            graders: vec![
                kraai_eval::ProcessRecord {
                    command: vec![String::from("grader-1")],
                    exit_code: Some(0),
                    timed_out: false,
                    output_limit_exceeded: false,
                    duration_ms: 1_000,
                },
                kraai_eval::ProcessRecord {
                    command: vec![String::from("grader-2")],
                    exit_code: Some(1),
                    timed_out: false,
                    output_limit_exceeded: false,
                    duration_ms: 2_000,
                },
            ],
            submission_sha256: Some(String::from("submission-hash")),
            started_at_ms: 1,
            completed_at_ms: 2,
            duration_ms: 62_345,
            model_proxy: None,
            metrics: kraai_eval::EvaluationMetrics::default(),
            controller_failure: None,
            provider_config_sha256: None,
            rust_environment_programs: None,
        };
        assert_eq!(
            format_result_summary(&result, PathBuf::from(".cache").as_path()),
            "Result: FAILED\nTask: task\nHarness: kraai\nVersion: git:abc\nModel: model\nAttempt: 1\nElapsed: 1m 2s\nRunner: passed (2.3s)\nGraders: 1/2 passed\nTokens: unavailable\nArtifacts: .cache/runs/task/kraai/git-abc/model/attempt-1/deadbeef"
        );
    }
}
