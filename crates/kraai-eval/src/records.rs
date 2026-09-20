use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{EvaluationMetrics, NetworkPolicy};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub schema_version: u32,
    pub experiment_id: String,
    pub artifact_path: PathBuf,
    pub task_id: String,
    pub harness_name: String,
    pub model_label: Option<String>,
    pub attempt: u64,
    pub runner_version: String,
    pub runner_artifact_sha256: String,
    pub task_sha256: String,
    pub grader_sha256: String,
    pub sandbox: SandboxRecord,
    pub status: RunStatus,
    pub runner: Option<ProcessRecord>,
    pub graders: Vec<ProcessRecord>,
    pub submission_sha256: Option<String>,
    pub started_at_ms: u128,
    pub completed_at_ms: u128,
    pub duration_ms: u128,
    pub model_proxy: Option<ProxyRecord>,
    pub metrics: EvaluationMetrics,
    pub controller_failure: Option<ControllerFailure>,
    pub provider_config_sha256: Option<String>,
    pub rust_environment_programs: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Passed,
    Failed,
    RunnerFailed,
    ControllerFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerFailure {
    pub phase: String,
    pub error: String,
    pub retained_work_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRecord {
    pub backend: String,
    pub network: NetworkPolicy,
    pub environment_cleared: bool,
    pub max_memory_bytes: u64,
    pub max_processes: u64,
    pub cpu_quota_percent: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRecord {
    #[serde(default)]
    pub transport_revision: u32,
    pub kind: String,
    pub upstream: String,
    pub allowed_paths: Vec<String>,
    pub max_requests: u64,
    pub credential_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessRecord {
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub output_limit_exceeded: bool,
    pub duration_ms: u128,
}
