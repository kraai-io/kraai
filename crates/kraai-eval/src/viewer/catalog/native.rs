use std::collections::BTreeSet;
use std::path::{Component, Path};

use super::{Attempt, Catalog, Metrics, Version};
use crate::{RunResult, RunStatus, SuiteResult};

impl Catalog {
    pub(super) fn native_runs(&mut self) {
        let root = self.root.clone();
        let mut experiments = BTreeSet::new();
        let directories = self.directories(&root.join("runs"), 6);
        for directory in directories {
            let Some(result) = self.json::<RunResult>(&directory.join("result.json")) else {
                self.incomplete_native(&directory);
                continue;
            };
            if experiments.insert(result.experiment_id.clone()) {
                self.native_run(&directory, result);
            }
        }
        let mut archived = Vec::new();
        for directory in self.directories(&root.join("failures"), 1) {
            if directory.join("result.json").exists()
                && let Some(result) = self.json::<RunResult>(&directory.join("result.json"))
            {
                archived.push((directory, result));
            }
        }
        archived.sort_by(|(left_path, left), (right_path, right)| {
            right
                .completed_at_ms
                .cmp(&left.completed_at_ms)
                .then_with(|| right.started_at_ms.cmp(&left.started_at_ms))
                .then_with(|| left_path.cmp(right_path))
        });
        for (directory, result) in archived {
            if experiments.insert(result.experiment_id.clone()) {
                self.native_run(&directory, result);
            }
        }
    }

    fn native_run(&mut self, directory: &Path, mut result: RunResult) {
        let error = failure(&result);
        if let Some(proxy) = result.metrics.proxy.as_mut() {
            self.hydrate_proxy(directory, proxy);
        }
        let manifest: serde_json::Value = self
            .optional_json(&directory.join("manifest.json"))
            .unwrap_or_default();
        let identity = serde_json::json!({
            "runner": result.runner_artifact_sha256,
            "runner_args": manifest.get("runner_args"),
            "provider_config": result.provider_config_sha256,
            "rust_environment_programs": result.rust_environment_programs,
            "sandbox": result.sandbox,
            "proxy": result.model_proxy.as_ref().map(|proxy| serde_json::json!({
                "transport_revision": proxy.transport_revision, "kind": proxy.kind,
                "upstream": proxy.upstream, "allowed_paths": proxy.allowed_paths, "max_requests": proxy.max_requests,
            })),
        }).to_string();
        let version_id = self.version(
            Version {
                id: String::new(),
                benchmark: String::from("custom"),
                harness: result.harness_name,
                model: result.model_label,
                label: result.runner_version,
                latest_at_ms: Some(result.started_at_ms),
            },
            &identity,
        );
        self.task_revision(
            &version_id,
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
                version_id,
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

    fn incomplete_native(&mut self, directory: &Path) {
        let Ok(relative) = directory.strip_prefix(self.root.join("runs")) else {
            return;
        };
        let parts = relative
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let Some(task) = parts.first() else { return };
        let Some(harness) = parts.get(1) else { return };
        let Some(label) = parts.get(2) else { return };
        let model = parts
            .get(3)
            .filter(|model| model.as_str() != "unlabeled-model")
            .cloned();
        let attempt = parts
            .get(4)
            .and_then(|part| part.strip_prefix("attempt-"))
            .and_then(|part| part.parse().ok())
            .unwrap_or_default();
        let version_id = self.version(
            Version {
                id: String::new(),
                benchmark: String::from("custom"),
                harness: harness.clone(),
                model,
                label: label.clone(),
                latest_at_ms: None,
            },
            "incomplete",
        );
        self.insert(
            directory,
            Attempt {
                id: self.source_id(directory),
                version_id,
                task: task.clone(),
                attempt,
                status: String::from("interrupted"),
                started_at_ms: None,
                metrics: Metrics::default(),
                error: Some(String::from("Saved run result is missing or unreadable")),
                logs: Vec::new(),
            },
        );
    }

    pub(super) fn native_suites(&mut self) {
        for directory in self.directories(&self.root.join("suites"), 4) {
            let Some(suite) = self.json::<SuiteResult>(&directory.join("summary.json")) else {
                continue;
            };
            for (index, run) in suite.runs.into_iter().enumerate() {
                if let Some(artifact) = run.artifact_path.as_ref() {
                    if artifact
                        .components()
                        .any(|component| !matches!(component, Component::Normal(_)))
                    {
                        self.warning(
                            &directory,
                            "Suite contains an invalid attempt artifact path",
                        );
                        continue;
                    }
                    if !self.root.join(artifact).join("result.json").exists() {
                        self.warning(
                            &directory,
                            format!("Saved attempt result is missing: {}", artifact.display()),
                        );
                    }
                    continue;
                }
                let source = directory.join(format!("launch-{index}"));
                let version_id = self.version(
                    Version {
                        id: String::new(),
                        benchmark: String::from("custom"),
                        harness: suite.harness_name.clone(),
                        model: suite.model_label.clone(),
                        label: suite.runner_version.clone(),
                        latest_at_ms: Some(suite.started_at_ms),
                    },
                    "launch-failed",
                );
                self.insert(
                    &source,
                    Attempt {
                        id: self.source_id(&source),
                        version_id,
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
