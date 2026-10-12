use crate::database::{Database, assert_owner, list_records, read_record, record_session};
use crate::history::read_history_chain;
use crate::message_references::{
    assert_message_owners, delete_message_record, write_message_record,
};
use crate::{SessionMeta, SessionStore};
use color_eyre::eyre::{Result, ensure};
use kraai_types::{Message, MessageId, SessionId};
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
    async fn read_conversation(
        &self,
        _session_id: &SessionId,
    ) -> Result<Option<ConversationSnapshot>> {
        Ok(None)
    }

    fn sqlite_database(&self) -> Option<crate::SqliteDatabase> {
        None
    }

    /// Get a message by ID
    async fn get(&self, id: &MessageId) -> Result<Option<Message>>;

    /// Reads up to `limit` messages from `from` toward the root, tip first.
    async fn read_history(&self, from: &MessageId, limit: usize) -> Result<Vec<Message>> {
        let mut history = Vec::new();
        let mut visited = HashSet::new();
        let mut cursor = Some(from.clone());
        while let Some(id) = cursor
            && history.len() < limit
        {
            if !visited.insert(id.clone()) {
                return Err(crate::history::history_cycle(&id));
            }
            let message = self
                .get(&id)
                .await?
                .ok_or_else(|| crate::history::missing_history_message(&id))?;
            cursor = message.parent_id.clone();
            history.push(message);
        }
        Ok(history)
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
    async fn read_conversation(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ConversationSnapshot>> {
        let session_id = session_id.clone();
        self.database
            .read(move |connection, _| {
                let transaction = connection.transaction()?;
                let Some(session) = crate::sessions::read_session(&transaction, &session_id)?
                else {
                    return Ok(None);
                };
                let history = match &session.tip_id {
                    Some(tip) => {
                        crate::history_by_id(read_history_chain(&transaction, tip, usize::MAX)?)
                    }
                    None => BTreeMap::new(),
                };
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
            .read(move |connection, _| read_record(connection, "message", &id))
            .await
    }

    async fn read_history(&self, from: &MessageId, limit: usize) -> Result<Vec<Message>> {
        let from = from.clone();
        self.database
            .read(move |connection, _| read_history_chain(connection, &from, limit))
            .await
    }

    async fn save(&self, message: &Message) -> Result<()> {
        let message = message.clone();
        self.database
            .transaction(move |transaction, leases| {
                let mut session = record_session(transaction, "message", message.id.as_str())?;
                let associated_session_exists: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
                    [session.as_ref().map(SessionId::as_str)],
                    |row| row.get(0),
                )?;
                if !associated_session_exists && let Some(parent) = &message.parent_id {
                    assert_message_owners(transaction, leases, parent.as_str(), None)?;
                    if session.is_none() {
                        session = record_session(transaction, "message", parent.as_str())?;
                    }
                }
                write_message_record(
                    transaction,
                    leases,
                    "message",
                    message.id.as_str(),
                    session.as_ref(),
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
                write_message_record(
                    transaction,
                    leases,
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
                delete_message_record(transaction, leases, "message", &id)
            })
            .await
    }

    async fn exists(&self, id: &MessageId) -> Result<bool> {
        Ok(self.get(id).await?.is_some())
    }
    async fn list_ids(&self) -> Result<HashSet<MessageId>> {
        self.database
            .read(|connection, _| {
                let mut statement =
                    connection.prepare("SELECT id FROM records WHERE kind = 'message'")?;
                let ids = statement.query_map([], |row| row.get::<_, String>(0))?;
                ids.map(|id| {
                    MessageId::try_new(id?).map_err(|error| color_eyre::eyre::eyre!(error))
                })
                .collect()
            })
            .await
    }
}
