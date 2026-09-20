use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, eyre};
use kraai_types::{Message, MessageId};
use tokio::fs;
use tokio::sync::RwLock;

use crate::FileCompactionStore;
use crate::atomic_file::atomic_write_with_outcome;
use crate::keyed_locks::KeyedLocks;

/// Trait for storing and retrieving messages
#[async_trait::async_trait]
pub trait MessageStore: Send + Sync {
    /// Get a message by ID (checks hot cache first, then cold storage)
    async fn get(&self, id: &MessageId) -> Result<Option<Message>>;

    async fn read_parent_id(&self, id: &MessageId) -> Result<Option<MessageId>> {
        Ok(self.get(id).await?.and_then(|message| message.parent_id))
    }

    /// Save a message (writes to cold storage immediately, adds to hot cache)
    async fn save(&self, message: &Message) -> Result<()>;

    /// Remove a message from hot cache (keeps cold storage)
    async fn unload(&self, id: &MessageId);

    /// Delete a message from both hot cache and cold storage
    async fn delete(&self, id: &MessageId) -> Result<()>;

    /// Check if message exists in cold storage
    async fn exists(&self, id: &MessageId) -> Result<bool>;

    /// List all message IDs that exist on disk
    async fn list_all_on_disk(&self) -> Result<HashSet<MessageId>>;

    /// List all message IDs currently in hot cache
    async fn list_hot(&self) -> Result<HashSet<MessageId>>;
}

/// File-based message store with hot cache and cold storage
pub struct FileMessageStore {
    /// Hot cache for frequently accessed messages
    hot: RwLock<HashMap<MessageId, Message>>,
    message_locks: KeyedLocks<MessageId>,
    /// Base directory for cold storage
    cold_dir: PathBuf,
    compactions: FileCompactionStore,
}

impl FileMessageStore {
    pub fn new(data_dir: &Path) -> Self {
        let cold_dir = data_dir.join("messages");
        Self {
            hot: RwLock::new(HashMap::new()),
            message_locks: KeyedLocks::default(),
            cold_dir,
            compactions: FileCompactionStore::new(data_dir),
        }
    }

    fn message_path(&self, id: &MessageId) -> Result<PathBuf> {
        let raw = id.as_str();
        if MessageId::try_new(raw).is_err()
            || Path::new(raw).is_absolute()
            || raw.contains(['/', '\\', ':'])
        {
            return Err(eyre!("Unsafe message id for persisted path: {raw:?}"));
        }

        let path = self.cold_dir.join(format!("{raw}.json"));
        if path.parent() != Some(self.cold_dir.as_path()) {
            return Err(eyre!("Message path escaped storage directory: {path:?}"));
        }
        Ok(path)
    }

    async fn read_cold_message(&self, id: &MessageId) -> Result<Option<Message>> {
        let path = self.message_path(id)?;
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)
            .await
            .with_context(|| format!("Failed to read message file: {:?}", path))?;

        let message = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse message file: {:?}", path))?;
        Ok(Some(message))
    }

    /// Ensure the messages directory exists
    async fn ensure_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.cold_dir)
            .await
            .with_context(|| format!("Failed to create messages directory: {:?}", self.cold_dir))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl MessageStore for FileMessageStore {
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        // Check hot cache first
        {
            let hot = self.hot.read().await;
            if let Some(msg) = hot.get(id) {
                return Ok(Some(msg.clone()));
            }
        }

        let _guard = self.message_locks.lock(id).await;
        if let Some(message) = self.hot.read().await.get(id) {
            return Ok(Some(message.clone()));
        }

        // Check cold storage
        let Some(msg) = self.read_cold_message(id).await? else {
            return Ok(None);
        };

        // Add to hot cache
        {
            let mut hot = self.hot.write().await;
            hot.insert(id.clone(), msg.clone());
        }

        Ok(Some(msg))
    }

    async fn read_parent_id(&self, id: &MessageId) -> Result<Option<MessageId>> {
        if let Some(message) = self.hot.read().await.get(id) {
            return Ok(message.parent_id.clone());
        }

        let _guard = self.message_locks.lock(id).await;
        if let Some(message) = self.hot.read().await.get(id) {
            return Ok(message.parent_id.clone());
        }
        Ok(self
            .read_cold_message(id)
            .await?
            .and_then(|message| message.parent_id))
    }

    async fn save(&self, message: &Message) -> Result<()> {
        let _guard = self.message_locks.lock(&message.id).await;
        self.ensure_dir().await?;

        let path = self.message_path(&message.id)?;
        let content = serde_json::to_string_pretty(message)
            .with_context(|| format!("Failed to serialize message: {}", message.id))?;

        async {
            let outcome = atomic_write_with_outcome(&path, content.as_bytes()).await?;
            {
                let mut hot = self.hot.write().await;
                hot.insert(message.id.clone(), message.clone());
            }
            outcome.into_result()
        }
        .await
        .with_context(|| format!("Failed to write message file: {path:?}"))
    }

    async fn unload(&self, id: &MessageId) {
        let _guard = self.message_locks.lock(id).await;
        let mut hot = self.hot.write().await;
        hot.remove(id);
    }

    async fn delete(&self, id: &MessageId) -> Result<()> {
        let _guard = self.message_locks.lock(id).await;
        self.compactions.delete(id).await?;
        // Remove from hot cache
        {
            let mut hot = self.hot.write().await;
            hot.remove(id);
        }

        // Remove from cold storage
        let path = self.message_path(id)?;
        if path.exists() {
            fs::remove_file(&path)
                .await
                .with_context(|| format!("Failed to delete message file: {:?}", path))?;
        }

        Ok(())
    }

    async fn exists(&self, id: &MessageId) -> Result<bool> {
        let path = self.message_path(id)?;
        Ok(path.exists())
    }

    async fn list_hot(&self) -> Result<HashSet<MessageId>> {
        let hot = self.hot.read().await;
        Ok(hot.keys().cloned().collect())
    }

    async fn list_all_on_disk(&self) -> Result<HashSet<MessageId>> {
        let mut ids = HashSet::new();

        if !self.cold_dir.exists() {
            return Ok(ids);
        }

        let mut entries = fs::read_dir(&self.cold_dir)
            .await
            .with_context(|| format!("Failed to read messages directory: {:?}", self.cold_dir))?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false)
                && let Some(stem) = path.file_stem()
                && let Some(id_str) = stem.to_str()
            {
                let id = MessageId::try_new(id_str).map_err(|error| {
                    eyre!("Invalid message filename in storage {path:?}: {error}")
                })?;
                ids.insert(id);
            }
        }

        Ok(ids)
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "message persistence tests use direct fixture assertions"
)]
mod tests {
    use super::*;
    use kraai_types::{ConversationItem, MessageStatus};

    #[tokio::test]
    async fn ancestry_reads_validate_complete_messages_without_populating_hot_cache() {
        let directory =
            std::env::temp_dir().join(format!("kraai-message-ancestry-{}", ulid::Ulid::generate()));
        let store = FileMessageStore::new(&directory);
        let message = Message {
            id: MessageId::new("message"),
            parent_id: Some(MessageId::new("parent")),
            content: ConversationItem::User {
                text: String::from("hello"),
            },
            status: MessageStatus::Complete,
            agent_profile_id: None,
            generation: None,
        };
        store.save(&message).await.unwrap();
        store.unload(&message.id).await;

        assert_eq!(
            store.read_parent_id(&message.id).await.unwrap(),
            message.parent_id
        );
        assert!(store.list_hot().await.unwrap().is_empty());
        assert_eq!(
            store
                .read_parent_id(&MessageId::new("missing"))
                .await
                .unwrap(),
            None
        );

        let mut invalid = serde_json::to_value(&message).unwrap();
        invalid
            .as_object_mut()
            .unwrap()
            .insert(String::from("content"), serde_json::Value::Null);
        fs::write(
            store.message_path(&message.id).unwrap(),
            serde_json::to_vec(&invalid).unwrap(),
        )
        .await
        .unwrap();
        let error = store.read_parent_id(&message.id).await.unwrap_err();
        assert!(error.to_string().contains("Failed to parse message file"));
        assert!(store.list_hot().await.unwrap().is_empty());
        fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn cold_reads_and_deletes_do_not_leave_cached_messages() {
        let directory =
            std::env::temp_dir().join(format!("kraai-message-races-{}", ulid::Ulid::generate()));
        let store = FileMessageStore::new(&directory);
        let message = Message {
            id: MessageId::new("message"),
            parent_id: None,
            content: ConversationItem::User {
                text: String::from("hello"),
            },
            status: MessageStatus::Complete,
            agent_profile_id: None,
            generation: None,
        };

        for _ in 0..16 {
            store.save(&message).await.unwrap();
            store.unload(&message.id).await;

            let (loaded, deleted) = tokio::join!(store.get(&message.id), store.delete(&message.id));

            assert!(loaded.is_ok(), "{loaded:?}");
            deleted.unwrap();
            assert!(!store.exists(&message.id).await.unwrap());
            assert!(store.get(&message.id).await.unwrap().is_none());
            assert!(store.list_hot().await.unwrap().is_empty());
        }

        fs::remove_dir_all(directory).await.unwrap();
    }
}
