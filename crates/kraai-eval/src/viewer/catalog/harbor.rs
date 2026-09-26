use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use super::{Attempt, Catalog, Version};

impl Catalog {
    pub(super) fn harbor_jobs(&mut self) {
        for directory in self.directories(&self.root.join("public"), 5) {
            if directory
                .extension()
                .is_some_and(|extension| extension == "inputs")
            {
                continue;
            }
            self.harbor_job(&directory);
        }
    }

    fn harbor_job(&mut self, directory: &Path) {
        let run: Value = self
            .optional_json(&directory.join("kraai-run.json"))
            .unwrap_or(Value::Null);
        let lock: Value = self
            .optional_json(&directory.join("lock.json"))
            .unwrap_or(Value::Null);
        let job: Value = self
            .optional_json(&directory.join("result.json"))
            .unwrap_or(Value::Null);
        if run.is_null() && lock.is_null() && job.is_null() {
            return;
        }
        let parts = directory
            .strip_prefix(self.root.join("public"))
            .ok()
            .map(|path| {
                path.iter()
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let benchmark = string(&run, "/identity/dataset")
            .or_else(|| parts.first().cloned())
            .unwrap_or_else(|| String::from("Harbor"));
        let harness = string(&run, "/identity/spec/harness").or_else(|| parts.get(1).cloned());
        let label = parts.get(2).cloned();
        let model = string(&run, "/identity/model").or_else(|| parts.get(3).cloned());
        let mut trials = self
            .directories(directory, 1)
            .into_iter()
            .filter(|path| {
                path.join("result.json").exists()
                    || path.join("lock.json").exists()
                    || path.join("config.json").exists()
                    || path.join("trial.log").exists()
            })
            .collect::<Vec<_>>();
        trials.sort();
        let planned = lock.get("trials").and_then(Value::as_array).map(Vec::len);
        let completed = job
            .pointer("/stats/n_completed_trials")
            .and_then(Value::as_u64);
        if job.get("finished_at").is_none_or(Value::is_null)
            || planned.is_some_and(|planned| planned > trials.len())
            || planned
                .zip(completed)
                .is_some_and(|(planned, completed)| planned as u64 > completed)
        {
            self.warning(
                directory,
                "Harbor job is incomplete; showing saved trials only",
            );
        }
        let mut counts = BTreeMap::<String, u64>::new();
        let first = self.attempts.len();
        for trial in trials {
            let result: Value = self
                .optional_json(&trial.join("result.json"))
                .unwrap_or(Value::Null);
            let trial_lock: Value = self
                .optional_json(&trial.join("lock.json"))
                .unwrap_or(Value::Null);
            let config: Value = if trial_lock.is_null() {
                self.optional_json(&trial.join("config.json"))
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            let task = string(&result, "/task_name")
                .or_else(|| string(&trial_lock, "/task/name"))
                .or_else(|| string(&config, "/task/name"))
                .unwrap_or_else(|| {
                    trial
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
            let started_at_ms = timestamp(&result, "/started_at");
            let settings = settings(&run, &lock, &trial_lock, &config, &result);
            let version_id = self.version(
                Version {
                    id: String::new(),
                    benchmark: benchmark.clone(),
                    harness: harness
                        .clone()
                        .or_else(|| string(&result, "/agent_info/name"))
                        .unwrap_or_else(|| String::from("unknown")),
                    model: model
                        .clone()
                        .or_else(|| string(&result, "/config/agent/model_name")),
                    label: label
                        .clone()
                        .or_else(|| string(&result, "/agent_info/version"))
                        .unwrap_or_else(|| String::from("unversioned")),
                    latest_at_ms: started_at_ms,
                },
                &settings.to_string(),
            );
            if let Some(revision) =
                string(&trial_lock, "/task/digest").or_else(|| string(&result, "/task_checksum"))
            {
                self.task_revision(&version_id, &task, revision, &trial);
            }
            let adapter = run.pointer("/identity/spec").is_some_and(Value::is_object)
                || result
                    .pointer("/agent_result/metadata/kraai_eval")
                    .is_some_and(Value::is_object)
                || trial_lock
                    .pointer("/agent/kwargs/spec_path")
                    .is_some_and(Value::is_string);
            let metrics = self.harbor_metrics(&trial, &result, adapter);
            let exception = string(&result, "/exception_info/exception_type");
            let reward = result
                .pointer("/verifier_result/rewards/reward")
                .and_then(Value::as_f64);
            let status = match (
                result
                    .get("finished_at")
                    .is_some_and(|value| !value.is_null()),
                reward,
                exception.as_deref(),
            ) {
                (_, _, Some("CancelledError")) => "interrupted",
                (_, Some(1.0), _) => "passed",
                (_, Some(0.0), _) => "failed",
                (
                    _,
                    _,
                    Some(
                        "AgentTimeoutError"
                        | "NonZeroAgentExitCodeError"
                        | "AgentSafetyRefusalError",
                    ),
                ) => "failed",
                (_, _, Some(_)) => "error",
                (false, _, _) => "interrupted",
                (true, _, _) => "error",
            };
            let error = string(&result, "/exception_info/exception_message")
                .or(exception)
                .or_else(|| {
                    (status == "interrupted").then(|| String::from("Trial has no completed result"))
                })
                .or_else(|| {
                    (status == "error").then(|| String::from("Trial has no binary reward"))
                });
            self.insert(
                &trial,
                Attempt {
                    id: self.source_id(&trial),
                    version_id,
                    task,
                    attempt: 0,
                    status: status.to_owned(),
                    started_at_ms,
                    metrics,
                    error,
                    logs: Vec::new(),
                },
            );
        }
        if let Some(attempts) = self.attempts.get_mut(first..) {
            attempts.sort_by(|left, right| {
                left.started_at_ms
                    .cmp(&right.started_at_ms)
                    .then_with(|| left.id.cmp(&right.id))
            });
            for attempt in attempts {
                let count = counts.entry(attempt.task.clone()).or_default();
                attempt.attempt = *count;
                *count += 1;
            }
        }
    }
}

fn settings(run: &Value, lock: &Value, trial: &Value, config: &Value, result: &Value) -> Value {
    let mut trial = if trial.is_null() {
        config.clone()
    } else {
        trial.clone()
    };
    if let Some(trial) = trial.as_object_mut() {
        for key in ["task", "trial_name", "trials_dir", "job_id"] {
            trial.remove(key);
        }
    }
    if let Some(kwargs) = trial
        .pointer_mut("/agent/kwargs")
        .and_then(Value::as_object_mut)
    {
        kwargs.remove("spec_path");
    }
    serde_json::json!({
        "trial": trial,
        "harbor": lock.get("harbor"),
        "concurrency": lock.get("n_concurrent_trials"),
        "retry": lock.get("retry"),
        "runner_args": run.pointer("/identity/spec/runner_args"),
        "proxy_command": run.pointer("/identity/spec/proxy_command").and_then(Value::as_array)
            .map(|command| command.iter().skip(1).collect::<Vec<_>>()),
        "oracle": run.pointer("/identity/oracle"),
        "docker_compose": run.pointer("/identity/docker_compose"),
        "allow_agent_host": run.pointer("/identity/allow_agent_host"),
        "agent_version": result.pointer("/agent_info/version"),
    })
}

fn string(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn timestamp(value: &Value, pointer: &str) -> Option<u128> {
    let value = value.pointer(pointer)?.as_str()?;
    let parsed = chrono::DateTime::parse_from_rfc3339(value).ok()?;
    u128::try_from(parsed.timestamp_millis()).ok()
}
