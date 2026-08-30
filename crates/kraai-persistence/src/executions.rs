use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Context, Result, eyre};
use kraai_types::{
    MessageId, SandboxCapabilities, ScriptExecutionId, ScriptExecutionPhase, ScriptExecutionStatus,
    ScriptOutputStream, ScriptProfileSnapshot, ToolCallId,
};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock};
use ulid::Ulid;

use crate::{atomic_write, sync_parent_directory};

const RECORD_FILE: &str = "record.json";
const SOURCE_FILE: &str = "source.nu";
const STDOUT_FILE: &str = "stdout.bin";
const STDERR_FILE: &str = "stderr.bin";

#[derive(Debug, Clone)]
pub struct NewScriptExecution {
    pub id: ScriptExecutionId,
    pub session_id: String,
    pub source_message_id: MessageId,
    pub call_id: ToolCallId,
    pub profile: ScriptProfileSnapshot,
    pub source: Vec<u8>,
    pub requested_capabilities: SandboxCapabilities,
    pub effective_capabilities: SandboxCapabilities,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptExecutionRecord {
    pub id: ScriptExecutionId,
    pub result_message_id: MessageId,
    pub session_id: String,
    pub source_message_id: MessageId,
    pub call_id: ToolCallId,
    pub profile: ScriptProfileSnapshot,
    pub requested_capabilities: SandboxCapabilities,
    pub effective_capabilities: SandboxCapabilities,
    pub timeout: Option<Duration>,
    pub phase: ScriptExecutionPhase,
    pub status: Option<ScriptExecutionStatus>,
    pub created_at_millis: u64,
    pub updated_at_millis: u64,
    pub exit_code: Option<i32>,
    pub sandbox_denied: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ScriptExecutionCompletion {
    pub status: ScriptExecutionStatus,
    pub exit_code: Option<i32>,
    pub sandbox_denied: bool,
    pub error: Option<String>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedScriptOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[async_trait::async_trait]
pub trait ScriptExecutionStore: Send + Sync {
    async fn create(&self, execution: NewScriptExecution) -> Result<ScriptExecutionRecord>;

    async fn get(&self, id: &ScriptExecutionId) -> Result<Option<ScriptExecutionRecord>>;

    async fn list_for_session(&self, session_id: &str) -> Result<Vec<ScriptExecutionRecord>>;

    async fn list_all(&self) -> Result<Vec<ScriptExecutionRecord>>;

    async fn read_source(&self, id: &ScriptExecutionId) -> Result<Vec<u8>>;

    async fn read_output(&self, id: &ScriptExecutionId) -> Result<PersistedScriptOutput>;

    async fn mark_awaiting_approval(&self, id: &ScriptExecutionId)
    -> Result<ScriptExecutionRecord>;

    async fn mark_running(&self, id: &ScriptExecutionId) -> Result<ScriptExecutionRecord>;

    /// Append and sync an output prefix while the execution remains active.
    async fn append_output(
        &self,
        id: &ScriptExecutionId,
        stream: ScriptOutputStream,
        bytes: &[u8],
    ) -> Result<()>;

    async fn finish(
        &self,
        id: &ScriptExecutionId,
        completion: ScriptExecutionCompletion,
    ) -> Result<ScriptExecutionRecord>;
}

pub struct FileScriptExecutionStore {
    executions_dir: PathBuf,
    execution_locks: RwLock<HashMap<ScriptExecutionId, Arc<Mutex<()>>>>,
}

impl FileScriptExecutionStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            executions_dir: data_dir.join("executions"),
            execution_locks: RwLock::new(HashMap::new()),
        }
    }

    async fn execution_lock(&self, id: &ScriptExecutionId) -> Arc<Mutex<()>> {
        if let Some(lock) = self.execution_locks.read().await.get(id).cloned() {
            return lock;
        }
        self.execution_locks
            .write()
            .await
            .entry(id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn execution_dir(&self, id: &ScriptExecutionId) -> Result<PathBuf> {
        ScriptExecutionId::try_new(id.as_str()).map_err(|error| eyre!(error))?;
        let path = self.executions_dir.join(id.as_str());
        if path.parent() != Some(self.executions_dir.as_path()) {
            return Err(eyre!("Execution path escaped storage directory: {path:?}"));
        }
        Ok(path)
    }

    async fn load_record(&self, id: &ScriptExecutionId) -> Result<ScriptExecutionRecord> {
        let path = self.execution_dir(id)?.join(RECORD_FILE);
        let bytes = fs::read(&path)
            .await
            .with_context(|| format!("Failed to read script execution record: {path:?}"))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("Failed to parse script execution record: {path:?}"))
    }

    async fn persist_record(
        &self,
        execution_dir: &Path,
        record: &ScriptExecutionRecord,
    ) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(record)
            .context("Failed to serialize script execution record")?;
        atomic_write(&execution_dir.join(RECORD_FILE), &bytes).await
    }

    async fn transition(
        &self,
        id: &ScriptExecutionId,
        expected: &[ScriptExecutionPhase],
        target: ScriptExecutionPhase,
    ) -> Result<ScriptExecutionRecord> {
        let lock = self.execution_lock(id).await;
        let _guard = lock.lock().await;
        let mut record = self.load_record(id).await?;
        require_phase(&record, expected)?;
        record.phase = target;
        record.updated_at_millis = now_millis();
        self.persist_record(&self.execution_dir(id)?, &record)
            .await?;
        Ok(record)
    }
}

#[async_trait::async_trait]
impl ScriptExecutionStore for FileScriptExecutionStore {
    async fn create(&self, execution: NewScriptExecution) -> Result<ScriptExecutionRecord> {
        let lock = self.execution_lock(&execution.id).await;
        let _guard = lock.lock().await;
        fs::create_dir_all(&self.executions_dir)
            .await
            .with_context(|| {
                format!(
                    "Failed to create script executions directory: {:?}",
                    self.executions_dir
                )
            })?;
        let execution_dir = self.execution_dir(&execution.id)?;
        fs::create_dir(&execution_dir).await.with_context(|| {
            format!("Failed to create unique script execution directory: {execution_dir:?}")
        })?;
        sync_parent_directory(&self.executions_dir).await?;

        atomic_write(&execution_dir.join(SOURCE_FILE), &execution.source).await?;
        atomic_write(&execution_dir.join(STDOUT_FILE), &[]).await?;
        atomic_write(&execution_dir.join(STDERR_FILE), &[]).await?;
        let timestamp = now_millis();
        let record = ScriptExecutionRecord {
            id: execution.id,
            result_message_id: MessageId::new(Ulid::generate()),
            session_id: execution.session_id,
            source_message_id: execution.source_message_id,
            call_id: execution.call_id,
            profile: execution.profile,
            requested_capabilities: execution.requested_capabilities,
            effective_capabilities: execution.effective_capabilities,
            timeout: execution.timeout,
            phase: ScriptExecutionPhase::Prepared,
            status: None,
            created_at_millis: timestamp,
            updated_at_millis: timestamp,
            exit_code: None,
            sandbox_denied: false,
            error: None,
        };
        self.persist_record(&execution_dir, &record).await?;
        Ok(record)
    }

    async fn get(&self, id: &ScriptExecutionId) -> Result<Option<ScriptExecutionRecord>> {
        let record_path = self.execution_dir(id)?.join(RECORD_FILE);
        match fs::metadata(&record_path).await {
            Ok(_) => self.load_record(id).await.map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| {
                format!("Failed to inspect script execution record: {record_path:?}")
            }),
        }
    }

    async fn list_for_session(&self, session_id: &str) -> Result<Vec<ScriptExecutionRecord>> {
        Ok(self
            .list_all()
            .await?
            .into_iter()
            .filter(|record| record.session_id == session_id)
            .collect())
    }

    async fn list_all(&self) -> Result<Vec<ScriptExecutionRecord>> {
        let mut records = Vec::new();
        let mut entries = match fs::read_dir(&self.executions_dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(records),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Failed to list script executions directory: {:?}",
                        self.executions_dir
                    )
                });
            }
        };
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let id = match entry.file_name().into_string() {
                Ok(id) => ScriptExecutionId::try_new(id),
                Err(_) => continue,
            };
            let Ok(id) = id else {
                continue;
            };
            let Some(record) = self.get(&id).await? else {
                continue;
            };
            records.push(record);
        }
        records.sort_by(|left, right| {
            (left.created_at_millis, &left.id).cmp(&(right.created_at_millis, &right.id))
        });
        Ok(records)
    }

    async fn read_source(&self, id: &ScriptExecutionId) -> Result<Vec<u8>> {
        let path = self.execution_dir(id)?.join(SOURCE_FILE);
        fs::read(&path)
            .await
            .with_context(|| format!("Failed to read script source: {path:?}"))
    }

    async fn read_output(&self, id: &ScriptExecutionId) -> Result<PersistedScriptOutput> {
        let execution_dir = self.execution_dir(id)?;
        let stdout_path = execution_dir.join(STDOUT_FILE);
        let stderr_path = execution_dir.join(STDERR_FILE);
        let (stdout, stderr) = tokio::try_join!(fs::read(&stdout_path), fs::read(&stderr_path))
            .with_context(|| format!("Failed to read script output from: {execution_dir:?}"))?;
        Ok(PersistedScriptOutput { stdout, stderr })
    }

    async fn mark_awaiting_approval(
        &self,
        id: &ScriptExecutionId,
    ) -> Result<ScriptExecutionRecord> {
        self.transition(
            id,
            &[ScriptExecutionPhase::Prepared],
            ScriptExecutionPhase::AwaitingApproval,
        )
        .await
    }

    async fn mark_running(&self, id: &ScriptExecutionId) -> Result<ScriptExecutionRecord> {
        self.transition(
            id,
            &[
                ScriptExecutionPhase::Prepared,
                ScriptExecutionPhase::AwaitingApproval,
            ],
            ScriptExecutionPhase::Running,
        )
        .await
    }

    async fn append_output(
        &self,
        id: &ScriptExecutionId,
        stream: ScriptOutputStream,
        bytes: &[u8],
    ) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let lock = self.execution_lock(id).await;
        let _guard = lock.lock().await;
        let record = self.load_record(id).await?;
        require_phase(&record, &[ScriptExecutionPhase::Running])?;
        let file_name = match stream {
            ScriptOutputStream::Stdout => STDOUT_FILE,
            ScriptOutputStream::Stderr => STDERR_FILE,
        };
        let path = self.execution_dir(id)?.join(file_name);
        let mut output = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .with_context(|| format!("Failed to open script output for append: {path:?}"))?;
        output
            .write_all(bytes)
            .await
            .with_context(|| format!("Failed to append script output: {path:?}"))?;
        output
            .flush()
            .await
            .with_context(|| format!("Failed to flush script output: {path:?}"))?;
        output
            .sync_data()
            .await
            .with_context(|| format!("Failed to sync script output: {path:?}"))?;
        Ok(())
    }

    async fn finish(
        &self,
        id: &ScriptExecutionId,
        completion: ScriptExecutionCompletion,
    ) -> Result<ScriptExecutionRecord> {
        let lock = self.execution_lock(id).await;
        let _guard = lock.lock().await;
        let mut record = self.load_record(id).await?;
        require_completion_transition(record.phase, completion.status, id)?;

        let execution_dir = self.execution_dir(id)?;
        atomic_write(&execution_dir.join(STDOUT_FILE), &completion.stdout).await?;
        atomic_write(&execution_dir.join(STDERR_FILE), &completion.stderr).await?;

        record.phase = ScriptExecutionPhase::Finished;
        record.status = Some(completion.status);
        record.exit_code = completion.exit_code;
        record.sandbox_denied = completion.sandbox_denied;
        record.error = completion.error;
        record.updated_at_millis = now_millis();
        self.persist_record(&execution_dir, &record).await?;
        Ok(record)
    }
}

fn require_phase(record: &ScriptExecutionRecord, expected: &[ScriptExecutionPhase]) -> Result<()> {
    if expected.contains(&record.phase) {
        return Ok(());
    }
    Err(eyre!(
        "Execution {} has status {:?}; expected one of {expected:?}",
        record.id,
        record.phase
    ))
}

fn require_completion_transition(
    current: ScriptExecutionPhase,
    target: ScriptExecutionStatus,
    id: &ScriptExecutionId,
) -> Result<()> {
    let valid = match target {
        ScriptExecutionStatus::Denied | ScriptExecutionStatus::InvalidScript => matches!(
            current,
            ScriptExecutionPhase::Prepared | ScriptExecutionPhase::AwaitingApproval
        ),
        ScriptExecutionStatus::FailedToStart
        | ScriptExecutionStatus::SandboxUnavailable
        | ScriptExecutionStatus::RuntimeError => {
            matches!(
                current,
                ScriptExecutionPhase::Prepared
                    | ScriptExecutionPhase::AwaitingApproval
                    | ScriptExecutionPhase::Running
            )
        }
        ScriptExecutionStatus::Cancelled => matches!(
            current,
            ScriptExecutionPhase::AwaitingApproval | ScriptExecutionPhase::Running
        ),
        ScriptExecutionStatus::Completed | ScriptExecutionStatus::TimedOut => {
            current == ScriptExecutionPhase::Running
        }
    };
    if valid {
        Ok(())
    } else {
        Err(eyre!(
            "Invalid script execution transition for {id}: {current:?} -> {target:?}"
        ))
    }
}

fn now_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "persistence tests use direct assertions for fixture setup and stored artifacts"
)]
mod tests {
    use super::*;
    use kraai_types::{SandboxCapability, ToolCallId};
    use ulid::Ulid;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kraai-executions-{name}-{}", Ulid::generate()))
    }

    fn execution(id: &ScriptExecutionId) -> NewScriptExecution {
        NewScriptExecution {
            id: id.clone(),
            session_id: String::from("session"),
            source_message_id: MessageId::new("message"),
            call_id: ToolCallId::new("call-1"),
            profile: ScriptProfileSnapshot {
                id: String::from("coding"),
                commands: Vec::new(),
                permissions: kraai_types::SandboxPermissionSet::new([
                    SandboxCapability::WorkspaceRead,
                ])
                .unwrap(),
                permission_rules: kraai_types::CapabilityPermissionRules::default(),
                escalation_policy: kraai_types::EscalationPolicy::Prompt,
                environment: kraai_types::EnvironmentPolicy::AllowList,
                nushell_startup: kraai_types::NushellStartup::Clean,
                path: kraai_types::PathPolicy::Inherit,
            },
            source: b"1 + 1".to_vec(),
            requested_capabilities: SandboxCapabilities::default(),
            effective_capabilities: SandboxCapabilities::new([SandboxCapability::WorkspaceRead])
                .unwrap(),
            timeout: Some(Duration::from_secs(10)),
        }
    }

    #[tokio::test]
    async fn output_is_written_before_terminal_record_is_exposed() {
        let data_dir = test_dir("terminal-output");
        let id = ScriptExecutionId::new(Ulid::generate());
        let store = FileScriptExecutionStore::new(&data_dir);
        store.create(execution(&id)).await.unwrap();
        store.mark_running(&id).await.unwrap();
        store
            .append_output(&id, ScriptOutputStream::Stdout, b"partial")
            .await
            .unwrap();
        let prefix = store.read_output(&id).await.unwrap();
        assert_eq!(prefix.stdout, b"partial");
        store
            .finish(
                &id,
                ScriptExecutionCompletion {
                    status: ScriptExecutionStatus::Completed,
                    exit_code: Some(0),
                    sandbox_denied: false,
                    error: None,
                    stdout: b"ok\0binary".to_vec(),
                    stderr: b"warning".to_vec(),
                },
            )
            .await
            .unwrap();

        let reopened = FileScriptExecutionStore::new(&data_dir);
        let record = reopened.get(&id).await.unwrap().unwrap();
        let output = reopened.read_output(&id).await.unwrap();
        assert_eq!(record.phase, ScriptExecutionPhase::Finished);
        assert_eq!(record.status, Some(ScriptExecutionStatus::Completed));
        assert_eq!(output.stdout, b"ok\0binary");
        assert_eq!(output.stderr, b"warning");
        let _ = fs::remove_dir_all(data_dir).await;
    }
}
