use std::path::Path;

use super::{Attempt, Catalog, Metrics};
use crate::{RunResult, RunStatus, SuiteResult};

impl Catalog {
    pub(super) fn native_folder(&mut self, folder: &Path, version_id: &str) {
        for directory in self.directories(folder, 2) {
            let is_attempt = directory
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("attempt-"));
            if !is_attempt {
                continue;
            }
            match self.json::<RunResult>(&directory.join("result.json")) {
                Some(result) => self.native_run(&directory, result, version_id),
                None => self.incomplete_native(&directory, version_id),
            }
        }
        let Some(suite) = self.optional_json::<SuiteResult>(&folder.join("summary.json")) else {
            return;
        };
        for (index, run) in suite.runs.into_iter().enumerate() {
            if run.artifact_path.is_some() {
                continue;
            }
            let source = folder.join(format!("launch-{index}"));
            self.insert(
                &source,
                Attempt {
                    id: self.source_id(&source),
                    version_id: version_id.to_owned(),
                    task: run.task_id.unwrap_or_else(|| String::from("unknown task")),
                    attempt: run.attempt,
                    status: run
                        .status
                        .as_ref()
                        .map(status)
                        .unwrap_or("error")
                        .to_owned(),
                    started_at_ms: Some(suite.started_at_ms),
                    metrics: Metrics {
                        duration_ms: run.duration_ms,
                        ..Metrics::default()
                    },
                    error: run.error,
                    logs: Vec::new(),
                },
            );
        }
    }

    fn native_run(&mut self, directory: &Path, mut result: RunResult, version_id: &str) {
        let error = failure(&result);
        if let Some(proxy) = result.metrics.proxy.as_mut() {
            self.hydrate_proxy(directory, proxy);
        }
        self.task_revision(
            version_id,
            &result.task_id,
            format!("{}:{}", result.task_sha256, result.grader_sha256),
            directory,
        );
        let mut metrics = self.native_metrics(directory, &result.metrics);
        metrics.duration_ms = Some(result.duration_ms);
        self.insert(
            directory,
            Attempt {
                id: self.source_id(directory),
                version_id: version_id.to_owned(),
                task: result.task_id,
                attempt: result.attempt,
                status: status(&result.status).to_owned(),
                started_at_ms: Some(result.started_at_ms),
                metrics,
                error,
                logs: Vec::new(),
            },
        );
    }

    fn incomplete_native(&mut self, directory: &Path, version_id: &str) {
        let name = |path: Option<&Path>| {
            path.and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let attempt = name(Some(directory))
            .strip_prefix("attempt-")
            .and_then(|number| number.parse().ok())
            .unwrap_or_default();
        self.insert(
            directory,
            Attempt {
                id: self.source_id(directory),
                version_id: version_id.to_owned(),
                task: name(directory.parent()),
                attempt,
                status: String::from("interrupted"),
                started_at_ms: None,
                metrics: Metrics::default(),
                error: Some(String::from("Saved run result is missing or unreadable")),
                logs: Vec::new(),
            },
        );
    }
}

fn failure(result: &RunResult) -> Option<String> {
    if let Some(failure) = result.controller_failure.as_ref() {
        return Some(failure.error.clone());
    }
    match result.status {
        RunStatus::RunnerFailed => result
            .runner
            .as_ref()
            .and_then(|runner| process_failure("Runner", runner)),
        RunStatus::Failed => result
            .graders
            .iter()
            .enumerate()
            .find_map(|(index, grader)| process_failure(&format!("Grader {index}"), grader)),
        RunStatus::Passed | RunStatus::ControllerFailed => None,
    }
}

fn process_failure(name: &str, process: &crate::ProcessRecord) -> Option<String> {
    if process.timed_out {
        Some(format!("{name} timed out"))
    } else if process.output_limit_exceeded {
        Some(format!("{name} exceeded its output limit"))
    } else {
        match process.exit_code {
            Some(0) => None,
            Some(code) => Some(format!("{name} exited with code {code}")),
            None => Some(format!("{name} stopped without an exit code")),
        }
    }
}

fn status(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Passed => "passed",
        RunStatus::Failed => "failed",
        RunStatus::RunnerFailed => "failed",
        RunStatus::ControllerFailed => "error",
    }
}
