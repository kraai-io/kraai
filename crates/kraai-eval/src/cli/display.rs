use kraai_eval::{ProgressReporter, RunResult, RunStatus, SuiteResult};
use std::io::{self, IsTerminal, Write};
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

pub(super) fn format_comparison(result: &kraai_eval::ComparisonResult) -> String {
    let mut summary = format!(
        "Left: {} @ {}\nRight: {} @ {}\nModel label: {}\nPairs: {} evaluated, {} invalid\nPassed: {} left, {} right\nWins: {} left, {} right; {} ties",
        result.left.harness_name,
        result.left.runner_version,
        result.right.harness_name,
        result.right.runner_version,
        result.left.model_label.as_deref().unwrap_or("unlabeled"),
        result.evaluated_pairs,
        result.invalid_pairs,
        result.left_passed,
        result.right_passed,
        result.left_wins,
        result.right_wins,
        result.ties,
    );
    summary.push_str("\n\nAll evaluated pairs, including failed attempts:");
    append_efficiency(
        &mut summary,
        &result.total_tokens,
        &result.runner_time_ms,
        &result.wall_time_ms,
        &result.usage,
    );
    summary.push_str(&result.request_metrics.display());
    summary.push_str(&format!(
        "\n\nBoth harnesses passed: {} pairs",
        result.both_passed,
    ));
    let passed = &result.both_passed_efficiency;
    append_efficiency(
        &mut summary,
        &passed.total_tokens,
        &passed.runner_time_ms,
        &passed.wall_time_ms,
        &passed.usage,
    );
    summary.push_str(&passed.request_metrics.display());
    summary.push_str(
        "\n\nTiming is observational; shared-machine load can affect wall and runner time.",
    );
    for run in &result.runs {
        summary.push_str(&format!(
            "\n{} attempt {}: {} left, {} right; {:?}",
            run.task_id,
            run.attempt,
            run.left_status.as_ref().map_or_else(
                || String::from("launch failed"),
                |status| format!("{status:?}")
            ),
            run.right_status.as_ref().map_or_else(
                || String::from("launch failed"),
                |status| format!("{status:?}")
            ),
            run.outcome,
        ));
    }
    summary
}

fn append_efficiency(
    summary: &mut String,
    total_tokens: &kraai_eval::PairedMetric,
    runner_time_ms: &kraai_eval::PairedMetric,
    wall_time_ms: &kraai_eval::PairedMetric,
    usage: &kraai_eval::PairedUsageMetrics,
) {
    for (name, metric) in [
        ("Total tokens", total_tokens),
        ("Uncached input tokens", &usage.uncached_input_tokens),
        ("Cached input tokens", &usage.cache_read_tokens),
        ("Output tokens excluding reasoning", &usage.output_tokens),
        ("Reasoning tokens", &usage.reasoning_tokens),
        ("Proxy requests", &usage.proxy_requests),
        ("Runner time (ms)", runner_time_ms),
        ("Wall time (ms)", wall_time_ms),
    ] {
        summary.push_str(&format!(
            "\n{name}: {} left mean, {} right mean, {} measured pairs",
            metric
                .left_mean
                .map_or_else(|| String::from("n/a"), |value| format!("{value:.1}")),
            metric
                .right_mean
                .map_or_else(|| String::from("n/a"), |value| format!("{value:.1}")),
            metric.samples,
        ));
    }
}

pub(super) struct ProgressDisplay {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    interactive: bool,
}

impl ProgressDisplay {
    pub(super) fn start(reporter: ProgressReporter) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let interactive = io::stderr().is_terminal();
        let thread = std::thread::spawn(move || {
            let started = Instant::now();
            let mut previous_line = String::new();
            while !thread_stop.load(Ordering::Relaxed) {
                let snapshot = reporter.snapshot();
                let identity = format!(
                    "{}:{}:{}",
                    snapshot.task_id, snapshot.attempt, snapshot.phase
                );
                if !snapshot.task_id.is_empty() && (interactive || identity != previous_line) {
                    let line = format_progress_line(started.elapsed(), &snapshot);
                    let mut stderr = io::stderr().lock();
                    if interactive {
                        let columns = terminal_size::terminal_size_of(io::stderr())
                            .map_or(80, |(terminal_size::Width(columns), _)| {
                                usize::from(columns)
                            });
                        let line = fit_progress_line(&line, columns.saturating_sub(1));
                        let _ = write!(stderr, "\r\x1b[2K{line}");
                        let _ = stderr.flush();
                    } else {
                        let _ = writeln!(stderr, "{line}");
                    }
                    previous_line = identity;
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

    pub(super) fn finish(mut self) {
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
        "[{hours:02}:{minutes:02}:{seconds:02}] task={} | attempt={} | {} | harness={}@{} | model={}",
        snapshot.task_id,
        snapshot.attempt,
        snapshot.phase,
        snapshot.harness_name,
        snapshot.runner_version,
        model,
    )
}

fn fit_progress_line(line: &str, columns: usize) -> String {
    let clean: String = line
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if clean.width() <= columns {
        return clean;
    }
    let suffix = if columns >= 3 { "..." } else { "" };
    let budget = columns.saturating_sub(suffix.len());
    let mut output = String::new();
    for character in clean.chars() {
        output.push(character);
        if output.width() > budget {
            output.pop();
            break;
        }
    }
    output.push_str(suffix);
    output
}

pub(super) fn format_result_summary(result: &RunResult, cache_dir: &std::path::Path) -> String {
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
    let accounting = result
        .metrics
        .proxy
        .as_ref()
        .and_then(|proxy| proxy.accounting.as_ref())
        .map_or_else(String::new, |accounting| {
            format!(
                "\n{}",
                super::accounting::format_accounting(&accounting.summary())
            )
        });
    let failure = result
        .controller_failure
        .as_ref()
        .map_or_else(String::new, |failure| {
            format!("\nController failure: {}: {}", failure.phase, failure.error)
        });
    format!(
        "Result: {status}\nTask: {}\nHarness: {}\nVersion: {}\nModel: {}\nAttempt: {}\nElapsed: {}\nRunner: {} ({})\nGraders: {}\nTokens: {}{}{accounting}\nArtifacts: {}",
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

pub(super) fn format_suite_summary(result: &SuiteResult, cache_dir: &std::path::Path) -> String {
    let success_rate = result.success_rate.map_or_else(
        || String::from("n/a"),
        |rate| format!("{:.1}%", rate * 100.0),
    );
    let token_mean = result
        .total_tokens
        .distribution
        .mean
        .map_or_else(|| String::from("n/a"), |mean| format!("{mean:.0}"));
    let mut output = format!(
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
    );
    for run in &result.runs {
        if let Some(accounting) = &run.accounting {
            output.push_str(&format!(
                "\n{} attempt {}:\n{}",
                run.task_id.as_deref().unwrap_or("unknown task"),
                run.attempt,
                super::accounting::format_accounting(accounting)
            ));
        }
    }
    output
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
            "[01:01:01] task=plural-files | attempt=2 | running grader 1/3 | harness=kraai@git:abc123 | model=gpt-test"
        );
    }

    #[test]
    fn progress_fits_narrow_terminals_without_wrapping_or_control_characters() {
        let snapshot = kraai_eval::ProgressSnapshot {
            task_id: String::from("event-stream"),
            harness_name: String::from("kraai"),
            runner_version: format!("sha256:{}", "f".repeat(64)),
            model_label: Some(String::from("gpt-test")),
            attempt: 2,
            phase: String::from("running harness"),
        };
        let line = format_progress_line(Duration::from_secs(1), &snapshot);
        for columns in [80, 120] {
            let fitted = fit_progress_line(&line, columns - 1);
            assert!(fitted.width() < columns);
            assert!(fitted.contains("event-stream"));
            assert!(fitted.contains("attempt=2"));
            assert!(fitted.contains("running harness"));
        }
        for columns in 0..16 {
            for text in [
                "任务 🦀\n\u{1b} long line",
                "1\u{fe0f}\u{20e3}1\u{fe0f}\u{20e3} long line",
            ] {
                let fitted = fit_progress_line(text, columns);
                assert!(fitted.width() <= columns);
                assert!(!fitted.chars().any(char::is_control));
            }
        }
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
