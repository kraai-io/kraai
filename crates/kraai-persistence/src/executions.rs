use crate::database::{Database, assert_owner, list_records, read_record, write_record};
use color_eyre::eyre::{Result, ensure, eyre};
use kraai_types::{
    MessageId, SandboxCapabilities, ScriptExecutionId, ScriptExecutionPhase, ScriptExecutionStatus,
    ScriptOutputStream, ScriptProfileSnapshot, ToolCallId,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ulid::Ulid;
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
    pub images: std::collections::BTreeMap<u64, kraai_types::ImageAttachment>,
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
    pub started_at_millis: Option<u64>,
    pub updated_at_millis: u64,
    pub exit_code: Option<i32>,
    pub sandbox_denied: bool,
    pub error: Option<String>,
}

impl ScriptExecutionRecord {
    pub fn outcome(&self) -> Result<kraai_types::ScriptExecutionOutcome> {
        Ok(kraai_types::ScriptExecutionOutcome {
            status: self
                .status
                .ok_or_else(|| eyre!("Execution {} has no terminal status", self.id))?,
            exit_code: self.exit_code,
        })
    }

    pub fn elapsed_millis(&self) -> Option<u64> {
        self.started_at_millis
            .map(|started| self.updated_at_millis.saturating_sub(started))
    }
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

    async fn append_image(
        &self,
        id: &ScriptExecutionId,
        sequence: u64,
        image: kraai_types::ImageAttachment,
    ) -> Result<()>;

    async fn mark_awaiting_approval(&self, id: &ScriptExecutionId)
    -> Result<ScriptExecutionRecord>;

    async fn mark_running(&self, id: &ScriptExecutionId) -> Result<ScriptExecutionRecord>;

    /// Append and sync an output prefix while the execution remains active.
    async fn append_output(
        &self,
        id: &ScriptExecutionId,
        stream: ScriptOutputStream,
        bytes: Vec<u8>,
    ) -> Result<()>;

    async fn finish(
        &self,
        id: &ScriptExecutionId,
        completion: ScriptExecutionCompletion,
    ) -> Result<ScriptExecutionRecord>;
}

pub struct SqliteScriptExecutionStore {
    database: Database,
}
impl SqliteScriptExecutionStore {
    pub fn new(data_dir: &Path) -> Self {
        Self::with_database(Database::new(data_dir))
    }
    pub(crate) fn with_database(database: Database) -> Self {
        Self { database }
    }

    async fn update(
        &self,
        id: &ScriptExecutionId,
        update: impl FnOnce(&Connection, &mut ScriptExecutionRecord) -> Result<()> + Send + 'static,
    ) -> Result<ScriptExecutionRecord> {
        let id = id.to_string();
        self.database
            .transaction(move |transaction, leases| {
                let mut record =
                    read_record::<ScriptExecutionRecord>(transaction, "execution", &id)?
                        .ok_or_else(|| eyre!("Execution not found: {id}"))?;
                assert_owner(transaction, leases, &record.session_id)?;
                update(transaction, &mut record)?;
                record.updated_at_millis = now_millis();
                write_record(
                    transaction,
                    "execution",
                    &id,
                    Some(&record.session_id),
                    &record,
                )?;
                Ok(record)
            })
            .await
    }

    async fn transition(
        &self,
        id: &ScriptExecutionId,
        expected: Vec<ScriptExecutionPhase>,
        target: ScriptExecutionPhase,
    ) -> Result<ScriptExecutionRecord> {
        self.update(id, move |_, record| {
            require_phase(record, &expected)?;
            record.phase = target;
            if target == ScriptExecutionPhase::Running {
                record.started_at_millis = Some(now_millis());
            }
            Ok(())
        })
        .await
    }
}

#[async_trait::async_trait]
impl ScriptExecutionStore for SqliteScriptExecutionStore {
    async fn create(&self, execution: NewScriptExecution) -> Result<ScriptExecutionRecord> {
        ScriptExecutionId::try_new(execution.id.as_str()).map_err(|error| eyre!(error))?;
        self.database
            .transaction(move |transaction, leases| {
                assert_owner(transaction, leases, &execution.session_id)?;
                ensure!(
                    read_record::<ScriptExecutionRecord>(
                        transaction,
                        "execution",
                        execution.id.as_str()
                    )?
                    .is_none(),
                    "Script execution already exists"
                );
                let timestamp = now_millis();
                let record = ScriptExecutionRecord {
                    id: execution.id,
                    result_message_id: MessageId::new(Ulid::generate()),
                    images: Default::default(),
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
                    started_at_millis: None,
                    updated_at_millis: timestamp,
                    exit_code: None,
                    sandbox_denied: false,
                    error: None,
                };
                transaction.execute(
                    "INSERT INTO execution_sources(execution_id, source) VALUES (?1, ?2)",
                    params![record.id.as_str(), execution.source],
                )?;
                write_record(
                    transaction,
                    "execution",
                    record.id.as_str(),
                    Some(&record.session_id),
                    &record,
                )?;
                Ok(record)
            })
            .await
    }
    async fn get(&self, id: &ScriptExecutionId) -> Result<Option<ScriptExecutionRecord>> {
        let id = id.to_string();
        self.database
            .run(move |connection, _| read_record(connection, "execution", &id))
            .await
    }
    async fn list_for_session(&self, session_id: &str) -> Result<Vec<ScriptExecutionRecord>> {
        let session = session_id.to_string();
        self.database
            .run(move |connection, _| list_records(connection, "execution", Some(&session)))
            .await
    }
    async fn list_all(&self) -> Result<Vec<ScriptExecutionRecord>> {
        self.database
            .run(|connection, _| list_records(connection, "execution", None))
            .await
    }
    async fn read_source(&self, id: &ScriptExecutionId) -> Result<Vec<u8>> {
        let id = id.to_string();
        self.database
            .run(move |connection, _| {
                Ok(connection.query_row(
                    "SELECT source FROM execution_sources WHERE execution_id = ?1",
                    [id],
                    |row| row.get(0),
                )?)
            })
            .await
    }
    async fn read_output(&self, id: &ScriptExecutionId) -> Result<PersistedScriptOutput> {
        let id = id.to_string();
        self.database.run(move |connection, _| {
            ensure!(read_record::<ScriptExecutionRecord>(connection, "execution", &id)?.is_some(), "Execution not found: {id}");
            let mut output = PersistedScriptOutput { stdout: Vec::new(), stderr: Vec::new() };
            let mut statement = connection.prepare("SELECT stream, bytes FROM execution_output WHERE execution_id = ?1 ORDER BY sequence")?;
            for row in statement.query_map([id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)))? {
                let (stream, bytes) = row?;
                match stream.as_str() {
                    "stdout" => output.stdout.extend(bytes),
                    "stderr" => output.stderr.extend(bytes),
                    _ => return Err(eyre!("Invalid stored output stream")),
                }
            }
            Ok(output)
        }).await
    }
    async fn append_image(
        &self,
        id: &ScriptExecutionId,
        sequence: u64,
        image: kraai_types::ImageAttachment,
    ) -> Result<()> {
        image.validate().map_err(|error| eyre!(error))?;
        ensure!(sequence > 0, "Image attachment sequence must be positive");
        self.update(id, move |_, record| {
            if let Some(existing) = record.images.get(&sequence) {
                ensure!(
                    existing == &image,
                    "Image attachment sequence reused with different content"
                );
                return Ok(());
            }
            require_phase(record, &[ScriptExecutionPhase::Running])?;
            ensure!(
                record.images.len() < kraai_types::image::MAX_IMAGE_ATTACHMENTS,
                "Too many image attachments in one script execution"
            );
            let total = record
                .images
                .values()
                .map(|image| image.byte_length)
                .try_fold(image.byte_length, u64::checked_add);
            ensure!(
                total.is_some_and(|total| total <= kraai_types::image::MAX_REQUEST_IMAGE_BYTES),
                "Image attachments exceed the execution byte limit"
            );
            record.images.insert(sequence, image);
            Ok(())
        })
        .await
        .map(|_| ())
    }
    async fn mark_awaiting_approval(
        &self,
        id: &ScriptExecutionId,
    ) -> Result<ScriptExecutionRecord> {
        self.transition(
            id,
            vec![ScriptExecutionPhase::Prepared],
            ScriptExecutionPhase::AwaitingApproval,
        )
        .await
    }
    async fn mark_running(&self, id: &ScriptExecutionId) -> Result<ScriptExecutionRecord> {
        self.transition(
            id,
            vec![
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
        bytes: Vec<u8>,
    ) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let stream = match stream {
            ScriptOutputStream::Stdout => "stdout",
            ScriptOutputStream::Stderr => "stderr",
        };
        self.update(id, move |connection, record| {
            require_phase(record, &[ScriptExecutionPhase::Running])?;
            connection.execute(
                "INSERT INTO execution_output(execution_id, stream, bytes) VALUES (?1, ?2, ?3)",
                params![record.id.as_str(), stream, bytes],
            )?;
            Ok(())
        })
        .await
        .map(|_| ())
    }
    async fn finish(
        &self,
        id: &ScriptExecutionId,
        completion: ScriptExecutionCompletion,
    ) -> Result<ScriptExecutionRecord> {
        self.update(id, move |connection, record| {
            require_completion_transition(record.phase, completion.status, &record.id)?;
            connection.execute("DELETE FROM execution_output WHERE execution_id = ?1", [record.id.as_str()])?;
            for (stream, bytes) in [("stdout", completion.stdout), ("stderr", completion.stderr)] {
                if !bytes.is_empty() {
                    connection.execute("INSERT INTO execution_output(execution_id, stream, bytes) VALUES (?1, ?2, ?3)", params![record.id.as_str(), stream, bytes])?;
                }
            }
            record.phase = ScriptExecutionPhase::Finished;
            record.status = Some(completion.status);
            record.exit_code = completion.exit_code;
            record.sandbox_denied = completion.sandbox_denied;
            record.error = completion.error;
            Ok(())
        }).await
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
        | ScriptExecutionStatus::HostUnavailable
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
    reason = "execution persistence tests use direct fixture assertions"
)]
mod tests;
