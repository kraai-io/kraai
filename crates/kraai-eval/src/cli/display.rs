use kraai_eval::{Prices, ProgressReporter, RunResult, SuiteResult};
use kraai_types::Usd;
use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

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

pub(super) type AttemptCosts = BTreeMap<(String, u64), Option<Usd>>;

pub(super) fn attempt_costs(results: &[RunResult], prices: &Prices) -> AttemptCosts {
    results
        .iter()
        .map(|result| {
            let cost = result
                .metrics
                .proxy
                .as_ref()
                .and_then(|proxy| proxy.accounting.as_ref())
                .and_then(|accounting| prices.accounting_cost(accounting));
            ((result.task_id.clone(), result.attempt), cost)
        })
        .collect()
}

fn total_cost(costs: &AttemptCosts) -> Option<Usd> {
    if costs.is_empty() {
        return None;
    }
    costs
        .values()
        .try_fold(0_u64, |total, cost| total.checked_add((*cost)?.0))
        .map(Usd)
}

fn dollars(cost: Usd) -> f64 {
    cost.0 as f64 / 1_000_000_000.0
}

pub(super) fn priced_summary_json(
    result: &SuiteResult,
    costs: &AttemptCosts,
) -> serde_json::Result<serde_json::Value> {
    let mut value = serde_json::to_value(result)?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            String::from("estimated_cost_usd"),
            serde_json::json!(total_cost(costs).map(dollars)),
        );
    }
    let runs = value
        .get_mut("runs")
        .and_then(serde_json::Value::as_array_mut)
        .into_iter()
        .flatten();
    for (run, summary) in runs.zip(&result.runs) {
        let cost = summary
            .task_id
            .clone()
            .and_then(|task| costs.get(&(task, summary.attempt)).copied().flatten());
        if let Some(run) = run.as_object_mut() {
            run.insert(
                String::from("estimated_cost_usd"),
                serde_json::json!(cost.map(dollars)),
            );
        }
    }
    Ok(value)
}

pub(super) fn format_suite_summary(
    result: &SuiteResult,
    cache_dir: &std::path::Path,
    costs: &AttemptCosts,
) -> String {
    let success_rate = result.success_rate.map_or_else(
        || String::from("n/a"),
        |rate| format!("{:.1}%", rate * 100.0),
    );
    let token_mean = result
        .total_tokens
        .distribution
        .mean
        .map_or_else(|| String::from("n/a"), |mean| format!("{mean:.0}"));
    let unpriced = costs.values().filter(|cost| cost.is_none()).count();
    let cost = total_cost(costs).map_or_else(
        || {
            format!(
                "unknown, {unpriced} of {} attempts have no cost",
                costs.len()
            )
        },
        |cost| format!("~{cost} at the model's saved price"),
    );
    let mut output = format!(
        "Attempts: {} total, {} evaluated\nResults: {} passed, {} failed, {} controller failures, {} launch failures\nSuccess rate: {}\nElapsed: {}\nTokens: {} total, {} mean per measured run\nEstimated cost: {}\nResults folder: {}",
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
        cost,
        cache_dir.join(&result.artifact_path).display(),
    );
    for run in &result.runs {
        if let Some(accounting) = &run.accounting {
            let task = run.task_id.as_deref().unwrap_or("unknown task");
            let cost = costs
                .get(&(task.to_owned(), run.attempt))
                .copied()
                .flatten();
            output.push_str(&format!(
                "\n{task} attempt {}:\n{}",
                run.attempt,
                format_accounting(accounting, cost)
            ));
        }
    }
    output
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

fn format_accounting(accounting: &kraai_eval::AccountingSummary, cost: Option<Usd>) -> String {
    let context = &accounting.context;
    let cost = cost.map_or_else(|| String::from("unknown"), |cost| format!("~{cost}"));
    format!(
        "Estimated cost: {cost}\nInput context: {} mean, {} peak, {} last recorded; {} measured / {} model requests",
        context
            .mean_input_tokens
            .map_or_else(|| String::from("unknown"), |value| format!("{value:.1}")),
        context
            .peak_input_tokens
            .map_or_else(|| String::from("unknown"), |value| value.to_string()),
        context
            .last_input_tokens
            .map_or_else(|| String::from("unknown"), |value| value.to_string()),
        context.samples,
        accounting.model_requests
    )
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
}
