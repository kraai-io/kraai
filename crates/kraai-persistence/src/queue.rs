use color_eyre::eyre::Result;
use kraai_types::{MessageContent, ModelSelection, SessionId};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::SqliteSessionStore;
use crate::database::assert_owner;

/// A user message waiting for the session's current work to finish.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub content: MessageContent,
    pub selection: ModelSelection,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueuedEntry {
    pub sequence: i64,
    pub message: QueuedMessage,
}

pub(crate) fn consume_queued_messages(
    connection: &Connection,
    session: &SessionId,
    through: i64,
) -> Result<()> {
    connection.execute(
        "DELETE FROM queued_messages WHERE session_id = ?1 AND sequence <= ?2",
        params![session.as_str(), through],
    )?;
    Ok(())
}

impl SqliteSessionStore {
    /// Queues a message and returns its one-based position.
    pub async fn enqueue_message(
        &self,
        session_id: &SessionId,
        message: &QueuedMessage,
    ) -> Result<usize> {
        let (session, data) = (session_id.clone(), serde_json::to_string(message)?);
        self.database
            .transaction(move |transaction, leases| {
                assert_owner(transaction, leases, &session)?;
                transaction.execute(
                    "INSERT INTO queued_messages(session_id, sequence, data) VALUES (?1,
                     (SELECT COALESCE(MAX(sequence) + 1, 0) FROM queued_messages WHERE session_id = ?1), ?2)",
                    params![session.as_str(), data],
                )?;
                let count: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM queued_messages WHERE session_id = ?1",
                    [session.as_str()],
                    |row| row.get(0),
                )?;
                Ok(usize::try_from(count)?)
            })
            .await
    }

    pub async fn queued_messages(&self, session_id: &SessionId) -> Result<Vec<QueuedEntry>> {
        let session = session_id.clone();
        self.database
            .read(move |connection, _| {
                let mut statement = connection.prepare_cached(
                    "SELECT sequence, data FROM queued_messages WHERE session_id = ?1 ORDER BY sequence",
                )?;
                let rows = statement.query_map([session.as_str()], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?;
                rows.map(|row| {
                    let (sequence, data) = row?;
                    Ok(QueuedEntry {
                        sequence,
                        message: serde_json::from_str(&data)?,
                    })
                })
                .collect()
            })
            .await
    }

    pub async fn has_queued_messages(&self, session_id: &SessionId) -> Result<bool> {
        let session = session_id.clone();
        self.database
            .read(move |connection, _| {
                Ok(connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM queued_messages WHERE session_id = ?1)",
                    [session.as_str()],
                    |row| row.get(0),
                )?)
            })
            .await
    }

    /// Discards every queued message and reports whether any existed.
    pub async fn clear_queued_messages(&self, session_id: &SessionId) -> Result<bool> {
        let session = session_id.clone();
        self.database
            .transaction(move |transaction, leases| {
                assert_owner(transaction, leases, &session)?;
                Ok(transaction.execute(
                    "DELETE FROM queued_messages WHERE session_id = ?1",
                    [session.as_str()],
                )? > 0)
            })
            .await
    }
}
