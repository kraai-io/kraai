use color_eyre::eyre::{Context, ContextCompat, Result};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::sync::RwLock;
use types::{Message, MessageId};

/// Metadata for a session, persisted to disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub tip_id: Option<MessageId>,
    pub created_at: u64,
    pub updated_at: u64,
    pub title: Option<String>,
}

/// Trait for storing and retrieving messages
#[async_trait::async_trait]
pub trait MessageStore: Send + Sync {
    /// Get a message by ID (checks hot cache first, then cold storage)
    async fn get(&self, id: &MessageId) -> Result<Option<Message>>;

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

/// Trait for storing and retrieving sessions
#[async_trait::async_trait]
pub trait SessionStore: Send + Sync {
    /// List all sessions
    async fn list(&self) -> Result<Vec<SessionMeta>>;

    /// Get a session by ID
    async fn get(&self, id: &str) -> Result<Option<SessionMeta>>;

    /// Save a session
    async fn save(&self, session: &SessionMeta) -> Result<()>;

    /// Delete a session
    async fn delete(&self, id: &str) -> Result<()>;
}

/// File-based message store with hot cache and cold storage
pub struct FileMessageStore {
    /// Hot cache for frequently accessed messages
    hot: RwLock<HashMap<MessageId, Message>>,
    /// Base directory for cold storage
    cold_dir: PathBuf,
}

impl FileMessageStore {
    pub fn new(data_dir: &Path) -> Self {
        let cold_dir = data_dir.join("messages");
        Self {
            hot: RwLock::new(HashMap::new()),
            cold_dir,
        }
    }

    fn message_path(&self, id: &MessageId) -> PathBuf {
        self.cold_dir.join(format!("{}.json", id))
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

        // Check cold storage
        let path = self.message_path(id);
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)
            .await
            .with_context(|| format!("Failed to read message file: {:?}", path))?;

        let msg: Message = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse message file: {:?}", path))?;

        // Add to hot cache
        {
            let mut hot = self.hot.write().await;
            hot.insert(id.clone(), msg.clone());
        }

        Ok(Some(msg))
    }

    async fn save(&self, message: &Message) -> Result<()> {
        self.ensure_dir().await?;

        let path = self.message_path(&message.id);
        let content = serde_json::to_string_pretty(message)
            .with_context(|| format!("Failed to serialize message: {}", message.id))?;

        fs::write(&path, &content)
            .await
            .with_context(|| format!("Failed to write message file: {:?}", path))?;

        // Add to hot cache
        {
            let mut hot = self.hot.write().await;
            hot.insert(message.id.clone(), message.clone());
        }

        Ok(())
    }

    async fn unload(&self, id: &MessageId) {
        let mut hot = self.hot.write().await;
        hot.remove(id);
    }

    async fn delete(&self, id: &MessageId) -> Result<()> {
        // Remove from hot cache
        {
            let mut hot = self.hot.write().await;
            hot.remove(id);
        }

        // Remove from cold storage
        let path = self.message_path(id);
        if path.exists() {
            fs::remove_file(&path)
                .await
                .with_context(|| format!("Failed to delete message file: {:?}", path))?;
        }

        Ok(())
    }

    async fn exists(&self, id: &MessageId) -> Result<bool> {
        let path = self.message_path(id);
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
                ids.insert(MessageId::new(id_str));
            }
        }

        Ok(ids)
    }
}

/// File-based session store
pub struct FileSessionStore {
    /// Sessions metadata
    sessions: RwLock<HashMap<String, SessionMeta>>,
    /// Path to sessions file
    sessions_path: PathBuf,
    /// Reference to message store for GC
    message_store: Arc<dyn MessageStore>,
}

impl FileSessionStore {
    pub fn new(data_dir: &Path, message_store: Arc<dyn MessageStore>) -> Self {
        let sessions_path = data_dir.join("sessions.json");
        Self {
            sessions: RwLock::new(HashMap::new()),
            sessions_path,
            message_store,
        }
    }

    /// Load sessions from disk (should be called on startup)
    pub async fn load(&self) -> Result<()> {
        if !self.sessions_path.exists() {
            return Ok(());
        }

        let content = fs::read_to_string(&self.sessions_path)
            .await
            .with_context(|| format!("Failed to read sessions file: {:?}", self.sessions_path))?;

        let sessions: HashMap<String, SessionMeta> =
            serde_json::from_str(&content).with_context(|| "Failed to parse sessions file")?;

        let mut loaded = self.sessions.write().await;
        *loaded = sessions;

        Ok(())
    }

    /// Persist sessions to disk (internal version that takes sessions map)
    async fn persist_sessions(sessions: &HashMap<String, SessionMeta>, path: &Path) -> Result<()> {
        let content = serde_json::to_string_pretty(sessions)
            .with_context(|| "Failed to serialize sessions")?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directory: {:?}", parent))?;
        }

        // Write to temp file, then rename for atomicity
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, &content)
            .await
            .with_context(|| format!("Failed to write temp file: {:?}", temp_path))?;

        fs::rename(&temp_path, path)
            .await
            .with_context(|| format!("Failed to rename temp file to: {:?}", path))?;

        Ok(())
    }

    /// Collect all message IDs in a session's tree (from tip to root)
    async fn collect_tree_messages(&self, tip_id: &MessageId) -> Result<HashSet<MessageId>> {
        let mut messages = HashSet::new();
        let mut current = Some(tip_id.clone());

        while let Some(id) = current {
            messages.insert(id.clone());
            if let Some(msg) = self.message_store.get(&id).await? {
                current = msg.parent_id;
            } else {
                break;
            }
        }

        Ok(messages)
    }

    /// Collect all message IDs referenced by all sessions
    async fn collect_all_referenced_messages(&self) -> Result<HashSet<MessageId>> {
        let sessions = self.sessions.read().await;
        let mut all_messages = HashSet::new();

        for session in sessions.values() {
            if let Some(tip_id) = &session.tip_id {
                let tree = self.collect_tree_messages(tip_id).await?;
                all_messages.extend(tree);
            }
        }

        Ok(all_messages)
    }

    /// Garbage collect orphaned messages after deleting a session
    pub async fn gc_orphaned_messages(&self, deleted_tree: HashSet<MessageId>) -> Result<()> {
        let still_referenced = self.collect_all_referenced_messages().await?;

        let mut errors = Vec::new();
        for msg_id in deleted_tree {
            if !still_referenced.contains(&msg_id)
                && let Err(e) = self.message_store.delete(&msg_id).await
            {
                errors.push((msg_id, e));
            }
        }

        if !errors.is_empty() {
            for (id, e) in &errors {
                tracing::error!("Failed to delete orphaned message {}: {}", id, e);
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl SessionStore for FileSessionStore {
    async fn list(&self) -> Result<Vec<SessionMeta>> {
        let sessions = self.sessions.read().await;
        let mut list: Vec<_> = sessions.values().cloned().collect();
        list.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        Ok(list)
    }

    async fn get(&self, id: &str) -> Result<Option<SessionMeta>> {
        let sessions = self.sessions.read().await;
        Ok(sessions.get(id).cloned())
    }

    async fn save(&self, session: &SessionMeta) -> Result<()> {
        let mut sessions = self.sessions.write().await;
        sessions.insert(session.id.clone(), session.clone());
        Self::persist_sessions(&sessions, &self.sessions_path).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        // Get tip_id and clone sessions map under lock
        let (tip_id_to_delete, sessions_without_deleted) = {
            let sessions = self.sessions.read().await;
            let tip_id = sessions.get(id).and_then(|s| s.tip_id.clone());

            // Clone the map without the deleted session
            let mut new_sessions = sessions.clone();
            new_sessions.remove(id);

            (tip_id, new_sessions)
        };

        // Collect tree messages outside of lock (does I/O)
        let tree_to_delete = if let Some(tip_id) = tip_id_to_delete {
            Some(self.collect_tree_messages(&tip_id).await?)
        } else {
            None
        };

        // Persist without holding any lock
        Self::persist_sessions(&sessions_without_deleted, &self.sessions_path).await?;

        // Update in-memory map
        {
            let mut sessions = self.sessions.write().await;
            *sessions = sessions_without_deleted;
        }

        // GC orphaned messages (no lock held)
        if let Some(tree) = tree_to_delete {
            self.gc_orphaned_messages(tree).await?;
        }

        Ok(())
    }
}

/// Get the data directory for the application
pub fn get_data_dir() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("Failed to determine home directory")?;
    Ok(base_dirs.home_dir().join(".agent-desktop/data"))
}

impl FileSessionStore {
    /// Clean up orphaned messages (messages on disk not referenced by any session)
    pub async fn cleanup_orphans(&self) -> Result<usize> {
        let on_disk = self.message_store.list_all_on_disk().await?;
        let referenced = self.collect_all_referenced_messages().await?;

        let mut deleted_count = 0;
        for msg_id in on_disk.difference(&referenced) {
            match self.message_store.delete(msg_id).await {
                Ok(()) => deleted_count += 1,
                Err(e) => {
                    tracing::error!("Failed to delete orphaned message {}: {}", msg_id, e);
                }
            }
        }

        if deleted_count > 0 {
            tracing::info!("Cleaned up {} orphaned messages", deleted_count);
        }

        Ok(deleted_count)
    }
}

/// Initialize the persistence layer
pub async fn init() -> Result<(Arc<FileMessageStore>, Arc<FileSessionStore>)> {
    let data_dir = get_data_dir()?;
    fs::create_dir_all(&data_dir)
        .await
        .with_context(|| format!("Failed to create data directory: {:?}", data_dir))?;

    let message_store = Arc::new(FileMessageStore::new(&data_dir));
    let session_store = Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));

    session_store.load().await?;

    // Clean up any orphaned messages (e.g., from manually deleted sessions)
    session_store.cleanup_orphans().await?;

    Ok((message_store, session_store))
}
