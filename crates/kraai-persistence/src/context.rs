use crate::database::{Database, assert_owner, bump_revision, read_record, write_record};
use color_eyre::eyre::{Result, ensure, eyre};
use kraai_execution_types::{CommandInvocationId, ScriptExecutionId};
use kraai_types::{
    ContextStateEvent, ContextStateEventSource, ContextStateMutation, MessageId, PinnedFileScope,
    SessionId,
};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use ulid::Ulid;
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ContextStateDocument {
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub pinned: Vec<PinnedFile>,
    pub events: Vec<ContextStateEvent>,
    #[serde(default)]
    pub snapshots: Vec<FileContextSnapshot>,
    #[serde(default)]
    pub instructions: Vec<InstructionSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedFile {
    pub path: PathBuf,
    pub scope: PinnedFileScope,
    pub opened_event: String,
}

impl ContextStateDocument {
    pub fn pinned_files(&self) -> Vec<PinnedFile> {
        let mut pinned = self.pinned.clone();
        for event in &self.events {
            for mutation in &event.mutations {
                apply(&mut pinned, &event.id, mutation);
            }
        }
        pinned
    }

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
        self.revision = Some(event.id.clone());
        self.events.push(event);
    }

    /// Only a prefix can fold: replaying a later event before an earlier one could change the
    /// result.
    fn fold(&mut self, revertible: &HashSet<MessageId>) -> bool {
        let folded = self
            .events
            .iter()
            .take_while(|event| {
                event
                    .message_id
                    .as_ref()
                    .is_none_or(|message| !revertible.contains(message))
            })
            .count();
        for event in self.events.drain(..folded) {
            for mutation in &event.mutations {
                apply(&mut self.pinned, &event.id, mutation);
            }
        }
        folded > 0
    }

    fn revert(&mut self, messages: &HashSet<MessageId>) -> bool {
        let before = self.events.len();
        self.events.retain(|event| {
            event
                .message_id
                .as_ref()
                .is_none_or(|message| !messages.contains(message))
        });
        if self.events.len() == before {
            return false;
        }
        let pinned: HashSet<_> = self
            .pinned_files()
            .into_iter()
            .map(|file| (file.path, file.opened_event))
            .collect();
        self.snapshots.retain(|snapshot| {
            pinned.contains(&(snapshot.path.clone(), snapshot.opened_event.clone()))
        });
        self.revision = Some(Ulid::generate().to_string());
        true
    }
}

fn apply(pinned: &mut Vec<PinnedFile>, event_id: &str, mutation: &ContextStateMutation) {
    match mutation {
        ContextStateMutation::PinFile { path, scope } => {
            if let Some(existing) = pinned.iter_mut().find(|existing| &existing.path == path) {
                existing.scope = scope.clone();
            } else {
                pinned.push(PinnedFile {
                    path: path.clone(),
                    scope: scope.clone(),
                    opened_event: event_id.into(),
                });
            }
        }
        ContextStateMutation::UnpinFile { path, .. } => {
            pinned.retain(|existing| &existing.path != path);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileContextSnapshot {
    pub path: PathBuf,
    pub opened_event: String,
    pub anchor: MessageId,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionSnapshot {
    pub key: String,
    pub label: String,
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
        revision: Option<&str>,
        snapshots: Vec<FileContextSnapshot>,
        removals: Vec<ContextStateMutation>,
        revertible: &HashSet<MessageId>,
    ) -> Result<()>;

    async fn save_instructions(
        &self,
        session_id: &SessionId,
        instructions: Vec<InstructionSnapshot>,
    ) -> Result<()>;

    async fn revert_messages(&self, session_id: &SessionId, messages: &[MessageId]) -> Result<()>;

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
            let message_id = match &source {
                ContextStateEventSource::Command { execution_id, .. } => execution_source(transaction, execution_id)?,
                ContextStateEventSource::Runtime { .. } => None,
            };
            let event = ContextStateEvent { id: Ulid::generate().to_string(), source, mutations, message_id };
            document.push_event(event.clone());
            write_record(transaction, "context", session.as_str(), Some(&session), &document)?;
            Ok(event)
        }).await
    }
}

fn execution_source(
    connection: &rusqlite::Connection,
    execution_id: &ScriptExecutionId,
) -> Result<Option<MessageId>> {
    connection
        .query_row(
            "SELECT json_extract(data, '$.source_message_id') FROM records
             WHERE kind = 'execution' AND id = ?1",
            [execution_id.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .map(|id| MessageId::try_new(id).map_err(|error| eyre!(error)))
        .transpose()
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
        revision: Option<&str>,
        snapshots: Vec<FileContextSnapshot>,
        removals: Vec<ContextStateMutation>,
        revertible: &HashSet<MessageId>,
    ) -> Result<()> {
        let session = session_id.clone();
        let revision = revision.map(str::to_string);
        let revertible = revertible.clone();
        self.database
            .transaction(move |transaction, leases| {
                assert_owner(transaction, leases, &session)?;
                let mut document =
                    read_record::<ContextStateDocument>(transaction, "context", session.as_str())?
                        .unwrap_or_default();
                ensure!(
                    document.revision == revision,
                    "File context changed while snapshots were being refreshed"
                );
                let folded = document.fold(&revertible);
                if !folded && document.snapshots == snapshots && removals.is_empty() {
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
                        message_id: None,
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

    async fn revert_messages(&self, session_id: &SessionId, messages: &[MessageId]) -> Result<()> {
        let session = session_id.clone();
        let messages: HashSet<_> = messages.iter().cloned().collect();
        self.database
            .transaction(move |transaction, leases| {
                assert_owner(transaction, leases, &session)?;
                let Some(mut document) =
                    read_record::<ContextStateDocument>(transaction, "context", session.as_str())?
                else {
                    return Ok(());
                };
                if !document.revert(&messages) {
                    return Ok(());
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
mod tests;
