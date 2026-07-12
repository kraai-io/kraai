use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use color_eyre::eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::command::run_trusted;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskManifest {
    pub schema_version: u32,
    pub id: String,
    pub prompt: String,
    pub source: SourceSpec,
    #[serde(default)]
    pub runner: RunnerPolicy,
    pub grader: GraderSpec,
    #[serde(default = "default_max_submission_bytes")]
    pub max_submission_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpec {
    pub repository: PathBuf,
    pub revision: String,
    #[serde(default)]
    pub public_patch: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerPolicy {
    #[serde(default = "default_runner_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub network: NetworkPolicy,
    #[serde(default)]
    pub rust_toolchain: bool,
    #[serde(default = "default_max_memory_bytes")]
    pub max_memory_bytes: u64,
    #[serde(default = "default_max_processes")]
    pub max_processes: u64,
    #[serde(default = "default_cpu_quota_percent")]
    pub cpu_quota_percent: u64,
}

impl Default for RunnerPolicy {
    fn default() -> Self {
        Self {
            timeout_seconds: default_runner_timeout(),
            network: NetworkPolicy::Disabled,
            rust_toolchain: false,
            max_memory_bytes: default_max_memory_bytes(),
            max_processes: default_max_processes(),
            cpu_quota_percent: default_cpu_quota_percent(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    #[default]
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraderSpec {
    #[serde(default)]
    pub hidden_patch: Option<PathBuf>,
    pub commands: Vec<CommandSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub command: Vec<String>,
    #[serde(default = "default_command_timeout")]
    pub timeout_seconds: u64,
}

impl TaskManifest {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .wrap_err_with(|| format!("read task manifest {}", path.display()))?;
        toml::from_str(&contents)
            .wrap_err_with(|| format!("parse task manifest {}", path.display()))
    }

    pub fn validate(&self, task_dir: &Path) -> Result<()> {
        if self.schema_version != 1 {
            bail!("unsupported task schema version {}", self.schema_version);
        }
        if self.id.trim().is_empty() || self.prompt.trim().is_empty() {
            bail!("task id and prompt must not be empty");
        }
        if !self
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            bail!("task id may contain only ASCII letters, numbers, '-', '_', and '.'");
        }
        if self.runner.timeout_seconds == 0
            || self.runner.max_memory_bytes == 0
            || self.runner.max_processes == 0
            || self.runner.cpu_quota_percent == 0
            || self.max_submission_bytes == 0
        {
            bail!("timeouts, resource limits, and max_submission_bytes must be greater than zero");
        }
        if self.grader.commands.is_empty() {
            bail!("task must define at least one grader command");
        }
        for command in &self.grader.commands {
            if command.command.is_empty() || command.timeout_seconds == 0 {
                bail!("grader commands and timeouts must not be empty");
            }
        }
        let repository = resolve_repository(task_dir, &self.source.repository)?;
        if !repository.is_dir() {
            bail!("source repository does not exist: {}", repository.display());
        }
        if let Some(path) = &self.source.public_patch {
            resolve_public_path(task_dir, path)?;
        }
        if let Some(path) = &self.grader.hidden_patch {
            resolve_private_path(task_dir, path)?;
        }
        Ok(())
    }

    pub fn resolve_source_revision(&mut self, task_dir: &Path) -> Result<()> {
        let repository = resolve_repository(task_dir, &self.source.repository)?;
        let command = [
            String::from("git"),
            String::from("rev-parse"),
            String::from("--verify"),
            format!("{}^{{commit}}", self.source.revision),
        ];
        let outcome = run_trusted(&command, &repository, Duration::from_secs(30))?;
        if outcome.timed_out {
            bail!("resolving task source revision timed out after 30 seconds");
        }
        if !outcome.success() {
            bail!(
                "unable to resolve task source revision: {}",
                String::from_utf8_lossy(&outcome.stderr).trim()
            );
        }
        let revision = String::from_utf8(outcome.stdout)?.trim().to_owned();
        if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("git returned a non-canonical source revision: {revision}");
        }
        self.source.revision = revision;
        Ok(())
    }

    pub fn public_digest(&self, task_dir: &Path) -> Result<String> {
        let mut public = self.clone();
        public.source.repository = PathBuf::new();
        public.grader.hidden_patch = None;
        public.grader.commands.clear();
        let mut chunks = vec![toml::to_string(&public)?.into_bytes()];
        if let Some(path) = &self.source.public_patch {
            chunks.push(fs::read(resolve_public_path(task_dir, path)?)?);
        }
        Ok(crate::cache::hash_chunks(&chunks))
    }

    pub fn grader_digest(&self, task_dir: &Path) -> Result<String> {
        let mut chunks = vec![serde_json::to_vec(&self.grader)?];
        if let Some(path) = &self.grader.hidden_patch {
            chunks.push(fs::read(resolve_private_path(task_dir, path)?)?);
        }
        Ok(crate::cache::hash_chunks(&chunks))
    }
}

pub(crate) fn resolve_public_path(base: &Path, path: &Path) -> Result<PathBuf> {
    resolve_relative(base, path, "public task")
}

pub(crate) fn resolve_repository(base: &Path, path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(base.join(path))
    }
}

pub(crate) fn resolve_private_path(base: &Path, path: &Path) -> Result<PathBuf> {
    resolve_relative(base, path, "private grader")
}

fn resolve_relative(base: &Path, path: &Path, kind: &str) -> Result<PathBuf> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        bail!(
            "{kind} path must remain under the task directory: {}",
            path.display()
        );
    }
    let base = base.canonicalize()?;
    let unresolved = base.join(path);
    if !unresolved.exists() {
        bail!("{kind} path does not exist: {}", unresolved.display());
    }
    let resolved = unresolved.canonicalize()?;
    if !resolved.starts_with(&base) {
        bail!("{kind} path escapes the task directory: {}", path.display());
    }
    Ok(resolved)
}

const fn default_runner_timeout() -> u64 {
    600
}
const fn default_command_timeout() -> u64 {
    300
}
const fn default_max_submission_bytes() -> u64 {
    10 * 1024 * 1024
}
const fn default_max_memory_bytes() -> u64 {
    8 * 1024 * 1024 * 1024
}
const fn default_max_processes() -> u64 {
    512
}
const fn default_cpu_quota_percent() -> u64 {
    400
}
