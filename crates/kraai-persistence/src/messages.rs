use crate::database::{
    Database, assert_owner, list_records, read_record, record_session, write_record,
};
use crate::{SessionMeta, SessionStore};
use color_eyre::eyre::{Result, ensure};
use kraai_types::{Message, MessageId};
use rusqlite::params;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;
pub struct ConversationSnapshot {
    pub session: SessionMeta,
    pub history: BTreeMap<MessageId, Message>,
    pub requests: BTreeMap<MessageId, kraai_types::RequestUsage>,
}

#[async_trait::async_trait]
pub trait MessageStore: Send + Sync {
    async fn read_conversation(&self, _session_id: &str) -> Result<Option<ConversationSnapshot>> {
        Ok(None)
    }

    fn sqlite_database(&self) -> Option<crate::SqliteDatabase> {
        None
    }

    /// Get a message by ID
    async fn get(&self, id: &MessageId) -> Result<Option<Message>>;

    async fn read_parent_id(&self, id: &MessageId) -> Result<Option<MessageId>> {
        Ok(self.get(id).await?.and_then(|message| message.parent_id))
    }

    /// Save a message
    async fn save(&self, message: &Message) -> Result<()>;

    async fn save_linked(
        &self,
        message: &Message,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
        sessions: Arc<dyn SessionStore>,
    ) -> Result<bool> {
        self.save(message).await?;
        sessions.save_if_tip_matches(session, expected_tip).await
    }

    /// Delete a message
    async fn delete(&self, id: &MessageId) -> Result<()>;

    /// Check if a message exists
    async fn exists(&self, id: &MessageId) -> Result<bool>;

    /// List all message IDs
    async fn list_ids(&self) -> Result<HashSet<MessageId>>;
}

pub struct SqliteMessageStore {
    pub(crate) database: Database,
}

impl SqliteMessageStore {
    pub fn new(data_dir: &Path) -> Self {
        Self::with_database(Database::new(data_dir))
    }
    pub fn with_database(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait::async_trait]
impl MessageStore for SqliteMessageStore {
    async fn read_conversation(&self, session_id: &str) -> Result<Option<ConversationSnapshot>> {
        let session_id = session_id.to_string();
        self.database
            .run(move |connection, _| {
                let transaction = connection.transaction()?;
                let Some(session) = crate::sessions::read_session(&transaction, &session_id)?
                else {
                    return Ok(None);
                };
                let mut history = BTreeMap::new();
                let mut cursor = session.tip_id.clone();
                let mut visited = HashSet::new();
                while let Some(id) = cursor {
                    ensure!(
                        visited.insert(id.clone()),
                        "Corrupt message parent graph: cycle repeats message {id}"
                    );
                    let message = read_record::<Message>(&transaction, "message", id.as_str())?
                        .ok_or_else(|| {
                            color_eyre::eyre::eyre!("History references missing message {id}")
                        })?;
                    cursor = message.parent_id.clone();
                    history.entry(message.id.clone()).or_insert(message);
                }
                let requests = list_records::<kraai_types::RequestUsage>(
                    &transaction,
                    "usage",
                    Some(&session_id),
                )?
                .into_iter()
                .map(|request| (request.message_id.clone(), request))
                .collect();
                transaction.commit()?;
                Ok(Some(ConversationSnapshot {
                    session,
                    history,
                    requests,
                }))
            })
            .await
    }

    fn sqlite_database(&self) -> Option<crate::SqliteDatabase> {
        Some(self.database.clone())
    }

    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        let id = id.to_string();
        self.database
            .run(move |connection, _| read_record(connection, "message", &id))
            .await
    }

    async fn save(&self, message: &Message) -> Result<()> {
        let message = message.clone();
        self.database
            .transaction(move |transaction, leases| {
                let session =
                    record_session(transaction, "message", message.id.as_str())?.or(match &message
                        .parent_id
                    {
                        Some(parent) => record_session(transaction, "message", parent.as_str())?,
                        None => None,
                    });
                if let Some(session) = &session {
                    assert_owner(transaction, leases, session)?;
                }
                write_record(
                    transaction,
                    "message",
                    message.id.as_str(),
                    session.as_deref(),
                    &message,
                )
            })
            .await
    }

    async fn save_linked(
        &self,
        message: &Message,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
        _sessions: Arc<dyn SessionStore>,
    ) -> Result<bool> {
        let message = message.clone();
        let session = session.clone();
        let expected = expected_tip.cloned();
        self.database
            .transaction(move |transaction, leases| {
                assert_owner(transaction, leases, &session.id)?;
                let current = crate::sessions::read_session(transaction, &session.id)?;
                if current
                    .as_ref()
                    .is_none_or(|current| current.tip_id != expected)
                {
                    return Ok(false);
                }
                crate::sessions::assert_session_revision(current.as_ref(), &session)?;
                if let Some(existing) =
                    read_record::<Message>(transaction, "message", message.id.as_str())?
                {
                    ensure!(
                        serde_json::to_value(&existing)? == serde_json::to_value(&message)?,
                        "Message ID reused with different content"
                    );
                }
                write_record(
                    transaction,
                    "message",
                    message.id.as_str(),
                    Some(&session.id),
                    &message,
                )?;
                crate::sessions::save_session(transaction, &session)?;
                Ok(true)
            })
            .await
    }

    async fn delete(&self, id: &MessageId) -> Result<()> {
        let id = id.to_string();
        self.database
            .transaction(move |transaction, leases| {
                if let Some(session) = record_session(transaction, "message", &id)? {
                    assert_owner(transaction, leases, &session)?;
                }
                transaction.execute(
                    "DELETE FROM records WHERE kind = 'message' AND id = ?1",
                    params![id],
                )?;
                Ok(())
            })
            .await
    }

    async fn exists(&self, id: &MessageId) -> Result<bool> {
        Ok(self.get(id).await?.is_some())
    }
    async fn list_ids(&self) -> Result<HashSet<MessageId>> {
        self.database
            .run(|connection, _| {
                Ok(list_records::<Message>(connection, "message", None)?
                    .into_iter()
                    .map(|message| message.id)
                    .collect())
            })
            .await
    }
}
