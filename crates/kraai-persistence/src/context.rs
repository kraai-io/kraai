use crate::database::{Database, assert_owner, bump_revision, read_record, write_record};
use color_eyre::eyre::{Result, ensure, eyre};
use kraai_execution_types::{CommandInvocationId, ScriptExecutionId};
use kraai_types::{
    ContextStateEvent, ContextStateEventSource, ContextStateMutation, MessageId, SessionId,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use ulid::Ulid;
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ContextStateDocument {
    pub events: Vec<ContextStateEvent>,
    #[serde(default)]
    pub snapshots: Vec<FileContextSnapshot>,
    #[serde(default)]
    pub instructions: Vec<InstructionSnapshot>,
}

impl ContextStateDocument {
    fn push_event(&mut self, event: ContextStateEvent) {
        let closed: HashSet<_> = event
            .mutations
            .iter()
            .filter_map(|mutation| match mutation {
                ContextStateMutation::UnpinFile { path, .. } => Some(path),
                ContextStateMutation::PinFile { .. } => None,
            })
            .collect();
        self.snapshots
            .retain(|snapshot| !closed.contains(&snapshot.path));
        self.events.push(event);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileContextSnapshot {
    pub path: PathBuf,
    pub opened_event: String,
    pub anchor: MessageId,
    pub text: String,
}

/// One instruction section, such as an AGENTS.md file, placed in the conversation like a pinned file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionSnapshot {
    pub key: String,
    pub label: String,
    /// The message this version follows; `None` places it before the whole conversation.
    pub anchor: Option<MessageId>,
    pub text: String,
}

#[async_trait::async_trait]
pub trait ContextStateStore: Send + Sync {
    async fn load(&self, session_id: &SessionId) -> Result<ContextStateDocument>;

    async fn list(&self, session_id: &SessionId) -> Result<Vec<ContextStateEvent>> {
        Ok(self.load(session_id).await?.events)
    }

    async fn append_command(
        &self,
        session_id: &SessionId,
        execution_id: &ScriptExecutionId,
        sequence: u64,
        invocation_id: &CommandInvocationId,
        command_id: &str,
        mutations: Vec<ContextStateMutation>,
    ) -> Result<ContextStateEvent>;

    async fn append_runtime(
        &self,
        session_id: &SessionId,
        component: &str,
        mutations: Vec<ContextStateMutation>,
    ) -> Result<ContextStateEvent>;

    async fn snapshots(&self, session_id: &SessionId) -> Result<Vec<FileContextSnapshot>> {
        Ok(self.load(session_id).await?.snapshots)
    }

    async fn save_snapshots(
        &self,
        session_id: &SessionId,
        through_event: Option<&str>,
        snapshots: Vec<FileContextSnapshot>,
        removals: Vec<ContextStateMutation>,
    ) -> Result<()>;

    async fn save_instructions(
        &self,
        session_id: &SessionId,
        instructions: Vec<InstructionSnapshot>,
    ) -> Result<()>;

    async fn delete(&self, session_id: &SessionId) -> Result<()>;
}

pub struct SqliteContextStateStore {
    database: Database,
}

impl SqliteContextStateStore {
    pub fn new(data_dir: &Path) -> Self {
        Self::with_database(Database::new(data_dir))
    }
    pub(crate) fn with_database(database: Database) -> Self {
        Self { database }
    }

    async fn append_event(
        &self,
        session_id: &SessionId,
        source: ContextStateEventSource,
        mutations: Vec<ContextStateMutation>,
    ) -> Result<ContextStateEvent> {
        ensure!(
            !mutations.is_empty(),
            "Context state events require at least one mutation"
        );
        let session = session_id.clone();
        self.database.transaction(move |transaction, leases| {
            assert_owner(transaction, leases, &session)?;
            let mut document = read_record::<ContextStateDocument>(transaction, "context", session.as_str())?.unwrap_or_default();
            if let ContextStateEventSource::Command { execution_id, sequence, invocation_id, .. } = &source {
                ensure!(!document.events.iter().any(|event| matches!(&event.source,
                    ContextStateEventSource::Command { execution_id: other_execution, sequence: other_sequence, invocation_id: other_invocation, .. }
                    if other_execution == execution_id && (other_sequence == sequence || other_invocation == invocation_id))),
                    "Context state effect {invocation_id} was already persisted for execution {execution_id}");
            }
            let event = ContextStateEvent { id: Ulid::generate().to_string(), source, mutations };
            document.push_event(event.clone());
            write_record(transaction, "context", session.as_str(), Some(&session), &document)?;
            Ok(event)
        }).await
    }
}

#[async_trait::async_trait]
impl ContextStateStore for SqliteContextStateStore {
    async fn load(&self, session_id: &SessionId) -> Result<ContextStateDocument> {
        let session = session_id.clone();
        self.database
            .read(move |connection, _| {
                Ok(read_record(connection, "context", session.as_str())?.unwrap_or_default())
            })
            .await
    }

    async fn append_command(
        &self,
        session_id: &SessionId,
        execution_id: &ScriptExecutionId,
        sequence: u64,
        invocation_id: &CommandInvocationId,
        command_id: &str,
        mutations: Vec<ContextStateMutation>,
    ) -> Result<ContextStateEvent> {
        self.append_event(
            session_id,
            ContextStateEventSource::Command {
                execution_id: execution_id.clone(),
                sequence,
                invocation_id: invocation_id.clone(),
                command_id: command_id.to_string(),
            },
            mutations,
        )
        .await
    }

    async fn append_runtime(
        &self,
        session_id: &SessionId,
        component: &str,
        mutations: Vec<ContextStateMutation>,
    ) -> Result<ContextStateEvent> {
        self.append_event(
            session_id,
            ContextStateEventSource::Runtime {
                component: component.to_string(),
            },
            mutations,
        )
        .await
    }

    async fn save_snapshots(
        &self,
        session_id: &SessionId,
        through_event: Option<&str>,
        snapshots: Vec<FileContextSnapshot>,
        removals: Vec<ContextStateMutation>,
    ) -> Result<()> {
        let session = session_id.clone();
        let through_event = through_event.map(str::to_string);
        self.database
            .transaction(move |transaction, leases| {
                assert_owner(transaction, leases, &session)?;
                let mut document =
                    read_record::<ContextStateDocument>(transaction, "context", session.as_str())?
                        .unwrap_or_default();
                ensure!(
                    document.events.last().map(|event| &event.id) == through_event.as_ref(),
                    "File context changed while snapshots were being refreshed"
                );
                if document.snapshots == snapshots && removals.is_empty() {
                    return Ok(());
                }
                for snapshot in &snapshots {
                    MessageId::try_new(snapshot.anchor.as_str()).map_err(|error| eyre!(error))?;
                }
                document.snapshots = snapshots;
                if !removals.is_empty() {
                    document.push_event(ContextStateEvent {
                        id: Ulid::generate().to_string(),
                        source: ContextStateEventSource::Runtime {
                            component: "file-context-refresh".into(),
                        },
                        mutations: removals,
                    });
                }
                write_record(
                    transaction,
                    "context",
                    session.as_str(),
                    Some(&session),
                    &document,
                )
            })
            .await
    }

    async fn save_instructions(
        &self,
        session_id: &SessionId,
        instructions: Vec<InstructionSnapshot>,
    ) -> Result<()> {
        let session = session_id.clone();
        self.database
            .transaction(move |transaction, leases| {
                assert_owner(transaction, leases, &session)?;
                let mut document =
                    read_record::<ContextStateDocument>(transaction, "context", session.as_str())?
                        .unwrap_or_default();
                if document.instructions == instructions {
                    return Ok(());
                }
                document.instructions = instructions;
                write_record(
                    transaction,
                    "context",
                    session.as_str(),
                    Some(&session),
                    &document,
                )
            })
            .await
    }

    async fn delete(&self, session_id: &SessionId) -> Result<()> {
        let session = session_id.clone();
        self.database
            .transaction(move |transaction, leases| {
                assert_owner(transaction, leases, &session)?;
                transaction.execute(
                    "DELETE FROM records WHERE kind = 'context' AND id = ?1",
                    [session.as_str()],
                )?;
                bump_revision(transaction, &session)
            })
            .await
    }
}

#[cfg(test)]
mod snapshot_tests;
