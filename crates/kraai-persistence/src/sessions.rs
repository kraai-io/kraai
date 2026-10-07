use crate::MessageStore;
use crate::database::{Database, LeaseTokens, assert_owner};
use crate::keyed_locks::KeyedLocks;
use color_eyre::eyre::{Result, ensure};
use kraai_types::{Message, MessageId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::OwnedMutexGuard;
/// Metadata for a session, persisted to disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    #[serde(skip)]
    pub revision: i64,
    pub id: String,
    pub tip_id: Option<MessageId>,
    pub workspace_dir: PathBuf,
    pub created_at: u64,
    pub updated_at: u64,
    pub title: Option<String>,
    #[serde(default)]
    pub selected_profile_id: Option<String>,
    #[serde(default)]
    pub selected_model: Option<kraai_types::ModelSelection>,
}

/// Trait for storing and retrieving sessions
#[async_trait::async_trait]
pub trait SessionStore: Send + Sync {
    /// List all sessions
    async fn list(&self) -> Result<Vec<SessionMeta>>;

    async fn list_ids(&self) -> Result<HashSet<String>> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .map(|session| session.id)
            .collect())
    }

    /// Get a session by ID
    async fn get(&self, id: &str) -> Result<Option<SessionMeta>>;

    /// Save a session
    async fn save(&self, session: &SessionMeta) -> Result<()>;

    /// Save a session only when its currently persisted tip matches `expected_tip`.
    async fn save_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
    ) -> Result<bool>;

    async fn link_message_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
    ) -> Result<bool>;

    async fn lock_message_mutation(&self, id: &MessageId) -> OwnedMutexGuard<()>;

    async fn delete_message_if_unreferenced(
        &self,
        id: &MessageId,
        message_store: Arc<dyn MessageStore>,
    ) -> Result<()>;

    /// Delete a session
    async fn delete(&self, id: &str) -> Result<()>;
}

pub struct SqliteSessionStore {
    pub(crate) database: Database,
    message_mutations: KeyedLocks<MessageId>,
}

impl SqliteSessionStore {
    pub fn new(data_dir: &Path, message_store: Arc<dyn MessageStore>) -> Self {
        Self::with_database(
            message_store
                .sqlite_database()
                .unwrap_or_else(|| Database::new(data_dir)),
        )
    }

    pub fn sqlite_database(&self) -> crate::SqliteDatabase {
        self.database.clone()
    }

    pub(crate) fn with_database(database: Database) -> Self {
        Self {
            database,
            message_mutations: KeyedLocks::default(),
        }
    }

    pub(crate) async fn load(&self) -> Result<()> {
        self.database.run(|_, _| Ok(())).await
    }

    async fn update(
        &self,
        session: &SessionMeta,
        expected: Option<Option<MessageId>>,
        require_message: bool,
    ) -> Result<bool> {
        let session = session.clone();
        self.database.transaction(move |transaction, leases| {
            assert_owner(transaction, leases, &session.id)?;
            let current = read_session(transaction, &session.id)?;
            if let Some(expected) = expected
                && current.as_ref().is_none_or(|current| current.tip_id != expected) {
                return Ok(false);
            }
            assert_session_revision(current.as_ref(), &session)?;
            if require_message && let Some(tip) = &session.tip_id {
                ensure!(crate::database::read_record::<Message>(transaction, "message", tip.as_str())?.is_some(),
                    "Cannot link missing message {tip}");
            }
            save_session(transaction, &session)?;
            if let Some(tip) = &session.tip_id {
                transaction.execute("UPDATE records SET session_id = COALESCE(session_id, ?1) WHERE kind = 'message' AND id = ?2",
                    params![session.id, tip.as_str()])?;
            }
            Ok(true)
        }).await
    }
}

pub(crate) fn assert_session_revision(
    current: Option<&SessionMeta>,
    updated: &SessionMeta,
) -> Result<()> {
    match current {
        Some(current) => ensure!(
            current.revision == updated.revision,
            kraai_types::DomainError::conflict("Session changed while updating metadata")
        ),
        None => ensure!(
            updated.revision == 0,
            kraai_types::DomainError::not_found(format!("Session not found: {}", updated.id))
        ),
    }
    Ok(())
}

pub(crate) fn read_session(connection: &Connection, id: &str) -> Result<Option<SessionMeta>> {
    let row: Option<(String, i64)> = connection
        .query_row(
            "SELECT data, metadata_revision FROM sessions WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(json, revision)| {
        let mut session: SessionMeta = serde_json::from_str(&json)?;
        session.revision = revision;
        Ok(session)
    })
    .transpose()
}

pub(crate) fn save_session(connection: &Connection, session: &SessionMeta) -> Result<()> {
    connection.execute("INSERT INTO sessions(id, data, tip_id, revision, metadata_revision) VALUES (?1, ?2, ?3, 1, 1)
        ON CONFLICT(id) DO UPDATE SET data = excluded.data, tip_id = excluded.tip_id, revision = sessions.revision + 1, metadata_revision = sessions.metadata_revision + 1",
        params![session.id, serde_json::to_string(session)?, session.tip_id.as_ref().map(MessageId::as_str)])?;
    Ok(())
}

pub(crate) fn message_referenced(connection: &Connection, id: &str) -> Result<bool> {
    Ok(connection.query_row("WITH RECURSIVE history(id) AS (
        SELECT tip_id FROM sessions WHERE tip_id IS NOT NULL
        UNION SELECT json_extract(records.data, '$.parent_id') FROM records JOIN history ON records.id = history.id
        WHERE records.kind = 'message' AND json_extract(records.data, '$.parent_id') IS NOT NULL
    ) SELECT EXISTS(SELECT 1 FROM history WHERE id = ?1)", [id], |row| row.get(0))?)
}

pub(crate) fn delete_unreferenced(
    connection: &Connection,
    leases: &LeaseTokens,
    id: &str,
) -> Result<()> {
    if message_referenced(connection, id)? {
        return Ok(());
    }
    if let Some(session) = crate::database::record_session(connection, "message", id)?
        && read_session(connection, &session)?.is_some()
    {
        assert_owner(connection, leases, &session)?;
    }
    connection.execute(
        "DELETE FROM records WHERE kind IN ('message', 'compaction') AND id = ?1",
        [id],
    )?;
    Ok(())
}

#[async_trait::async_trait]
impl SessionStore for SqliteSessionStore {
    async fn list(&self) -> Result<Vec<SessionMeta>> {
        self.database.run(|connection, _| {
            let mut statement = connection.prepare("SELECT data, metadata_revision FROM sessions ORDER BY json_extract(data, '$.updated_at') DESC, id")?;
            let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
            rows.map(|row| {
                let (json, revision) = row?;
                let mut session: SessionMeta = serde_json::from_str(&json)?;
                session.revision = revision;
                Ok(session)
            }).collect()
        }).await
    }

    async fn get(&self, id: &str) -> Result<Option<SessionMeta>> {
        let id = id.to_string();
        self.database
            .run(move |connection, _| read_session(connection, &id))
            .await
    }

    async fn save(&self, session: &SessionMeta) -> Result<()> {
        self.update(session, None, false).await.map(|_| ())
    }

    async fn save_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
    ) -> Result<bool> {
        self.update(session, Some(expected_tip.cloned()), false)
            .await
    }

    async fn link_message_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
    ) -> Result<bool> {
        self.update(session, Some(expected_tip.cloned()), true)
            .await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.database.transaction(move |transaction, leases| {
            assert_owner(transaction, leases, &id)?;
            let message_ids: Vec<String> = {
                let mut statement = transaction.prepare("WITH RECURSIVE history(id) AS (
                    SELECT tip_id FROM sessions WHERE id = ?1 AND tip_id IS NOT NULL
                    UNION SELECT json_extract(records.data, '$.parent_id') FROM records JOIN history ON records.id = history.id
                    WHERE records.kind = 'message' AND json_extract(records.data, '$.parent_id') IS NOT NULL
                ) SELECT id FROM history UNION SELECT id FROM records WHERE kind = 'message' AND session_id = ?1")?;
                statement.query_map([&id], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
            };
            transaction.execute("DELETE FROM sessions WHERE id = ?1", [&id])?;
            for message in message_ids { delete_unreferenced(transaction, leases, &message)?; }
            transaction.execute("DELETE FROM execution_output WHERE execution_id IN (SELECT id FROM records WHERE kind = 'execution' AND session_id = ?1)", [&id])?;
            transaction.execute("DELETE FROM execution_sources WHERE execution_id IN (SELECT id FROM records WHERE kind = 'execution' AND session_id = ?1)", [&id])?;
            transaction.execute("DELETE FROM records WHERE session_id = ?1 AND kind NOT IN ('message', 'compaction')", [&id])?;
            Ok(())
        }).await
    }

    async fn lock_message_mutation(&self, id: &MessageId) -> OwnedMutexGuard<()> {
        self.message_mutations.lock(id).await
    }

    async fn delete_message_if_unreferenced(
        &self,
        id: &MessageId,
        _message_store: Arc<dyn MessageStore>,
    ) -> Result<()> {
        let id = id.to_string();
        self.database
            .transaction(move |transaction, leases| delete_unreferenced(transaction, leases, &id))
            .await
    }
}
