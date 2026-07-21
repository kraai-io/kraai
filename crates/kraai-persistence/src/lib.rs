#![forbid(unsafe_code)]

use color_eyre::eyre::{Context, ContextCompat, Result, eyre};
use directories::BaseDirs;
use kraai_types::{Message, MessageId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock};
use ulid::Ulid;

mod context;
mod executions;
mod turns;

pub use context::{
    ContextStateEvent, ContextStateEventSource, ContextStateMutation, ContextStateStore,
    FileContextStateStore, PinnedFileScope,
};
pub use executions::{
    FileScriptExecutionStore, NewScriptExecution, PersistedScriptOutput, ScriptExecutionCompletion,
    ScriptExecutionRecord, ScriptExecutionStore,
};
pub use turns::{
    AppendMessageRequest, AppendedMessage, ConversationStore, IdempotentAppendOutcome,
};

/// Metadata for a session, persisted to disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub tip_id: Option<MessageId>,
    pub workspace_dir: PathBuf,
    pub created_at: u64,
    pub updated_at: u64,
    pub title: Option<String>,
    #[serde(default)]
    pub selected_profile_id: Option<String>,
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

    /// Save a session only when its currently persisted tip matches `expected_tip`.
    async fn save_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
    ) -> Result<bool>;

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

    /// Ensure the messages directory exists
    async fn ensure_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.cold_dir)
            .await
            .with_context(|| format!("Failed to create messages directory: {:?}", self.cold_dir))?;
        Ok(())
    }
}

/// Atomically replace `path` and make the acknowledged write crash-durable.
///
/// Unix persists both the file contents and containing directory entry. Other
/// platforms persist the file contents before replacement but may not expose a
/// portable directory-sync operation.
#[cfg(not(windows))]
pub(crate) async fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre!("Cannot atomically write path without a parent: {path:?}"))?;
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("Failed to create directory: {parent:?}"))?;

    let temp_path = temp_write_path(path);
    let write_result = async {
        let mut temp_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .await
            .with_context(|| format!("Failed to create temp file: {temp_path:?}"))?;
        temp_file
            .write_all(content)
            .await
            .with_context(|| format!("Failed to write temp file: {temp_path:?}"))?;
        temp_file
            .flush()
            .await
            .with_context(|| format!("Failed to flush temp file: {temp_path:?}"))?;
        temp_file
            .sync_all()
            .await
            .with_context(|| format!("Failed to sync temp file: {temp_path:?}"))?;
        drop(temp_file);

        fs::rename(&temp_path, path)
            .await
            .with_context(|| format!("Failed to rename temp file to: {path:?}"))?;
        sync_parent_directory(parent).await?;
        Ok(())
    }
    .await;

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path).await;
    }
    write_result
}

#[cfg(windows)]
pub(crate) async fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    use std::io::Write;

    let path = path.to_path_buf();
    let content = content.to_vec();
    tokio::task::spawn_blocking(move || {
        let parent = path
            .parent()
            .ok_or_else(|| eyre!("Cannot atomically write path without a parent: {path:?}"))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {parent:?}"))?;
        let mut file = atomic_write_file::AtomicWriteFile::open(&path)
            .with_context(|| format!("Failed to create atomic file for: {path:?}"))?;
        file.write_all(&content)
            .with_context(|| format!("Failed to write atomic file for: {path:?}"))?;
        file.commit()
            .with_context(|| format!("Failed to replace file atomically: {path:?}"))?;
        Ok(())
    })
    .await
    .map_err(|error| eyre!("Atomic write task failed: {error}"))?
}

#[cfg(unix)]
async fn sync_parent_directory(parent: &Path) -> Result<()> {
    let parent = parent.to_path_buf();
    tokio::task::spawn_blocking(move || {
        std::fs::File::open(&parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("Failed to sync parent directory: {parent:?}"))
    })
    .await
    .map_err(|error| eyre!("Parent directory sync task failed: {error}"))??;
    Ok(())
}

#[cfg(not(unix))]
async fn sync_parent_directory(_parent: &Path) -> Result<()> {
    Ok(())
}

fn temp_write_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("state"));
    path.with_file_name(format!(".{file_name}.{}.tmp", Ulid::new()))
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
        let path = self.message_path(id)?;
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

        let path = self.message_path(&message.id)?;
        let content = serde_json::to_string_pretty(message)
            .with_context(|| format!("Failed to serialize message: {}", message.id))?;

        atomic_write(&path, content.as_bytes())
            .await
            .with_context(|| format!("Failed to write message file: {path:?}"))?;

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

/// File-based session store
pub struct FileSessionStore {
    /// Sessions metadata
    sessions: RwLock<HashMap<String, SessionMeta>>,
    /// Serializes mutating session-store operations.
    write_guard: Mutex<()>,
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
            write_guard: Mutex::new(()),
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
        drop(loaded);

        Ok(())
    }

    /// Persist sessions to disk (internal version that takes sessions map)
    async fn persist_sessions(sessions: &HashMap<String, SessionMeta>, path: &Path) -> Result<()> {
        let content = serde_json::to_string_pretty(sessions)
            .with_context(|| "Failed to serialize sessions")?;

        atomic_write(path, content.as_bytes()).await
    }

    /// Collect all message IDs in a session's tree (from tip to root)
    async fn collect_tree_messages(&self, tip_id: &MessageId) -> Result<HashSet<MessageId>> {
        let mut messages = HashSet::new();
        let mut current = Some(tip_id.clone());

        while let Some(id) = current {
            if !messages.insert(id.clone()) {
                return Err(eyre!(
                    "Corrupt message parent graph: cycle repeats message {id}"
                ));
            }
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
        let sessions: Vec<SessionMeta> = self.sessions.read().await.values().cloned().collect();
        let mut all_messages = HashSet::new();

        for session in sessions {
            if let Some(tip_id) = &session.tip_id {
                let tree = self.collect_tree_messages(tip_id).await.with_context(|| {
                    format!("Failed to traverse messages for session {}", session.id)
                })?;
                all_messages.extend(tree);
            }
        }

        Ok(all_messages)
    }

    /// Garbage collect orphaned messages after deleting a session
    pub async fn gc_orphaned_messages(&self, deleted_tree: HashSet<MessageId>) -> Result<()> {
        let still_referenced = self.collect_all_referenced_messages().await?;

        let mut deleted_messages: Vec<_> = deleted_tree.into_iter().collect();
        deleted_messages.sort();

        let mut errors = Vec::new();
        for msg_id in deleted_messages {
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
            let detail = errors
                .into_iter()
                .map(|(id, error)| format!("{id}: {error}"))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(eyre!(
                "Failed to delete orphaned messages after session removal: {detail}"
            ));
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl SessionStore for FileSessionStore {
    async fn list(&self) -> Result<Vec<SessionMeta>> {
        let mut list: Vec<_> = self.sessions.read().await.values().cloned().collect();
        list.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        Ok(list)
    }

    async fn get(&self, id: &str) -> Result<Option<SessionMeta>> {
        let sessions = self.sessions.read().await;
        Ok(sessions.get(id).cloned())
    }

    async fn save(&self, session: &SessionMeta) -> Result<()> {
        let _write_guard = self.write_guard.lock().await;

        let mut next_sessions = self.sessions.read().await.clone();
        next_sessions.insert(session.id.clone(), session.clone());

        Self::persist_sessions(&next_sessions, &self.sessions_path).await?;

        let mut sessions = self.sessions.write().await;
        *sessions = next_sessions;
        drop(sessions);
        Ok(())
    }

    async fn save_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
    ) -> Result<bool> {
        let _write_guard = self.write_guard.lock().await;

        let mut next_sessions = self.sessions.read().await.clone();
        let Some(current) = next_sessions.get(&session.id) else {
            return Ok(false);
        };
        if current.tip_id.as_ref() != expected_tip {
            return Ok(false);
        }
        next_sessions.insert(session.id.clone(), session.clone());

        Self::persist_sessions(&next_sessions, &self.sessions_path).await?;

        let mut sessions = self.sessions.write().await;
        *sessions = next_sessions;
        drop(sessions);
        Ok(true)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let _write_guard = self.write_guard.lock().await;

        let current_sessions = self.sessions.read().await.clone();
        let tip_id_to_delete = current_sessions.get(id).and_then(|s| s.tip_id.clone());
        let mut sessions_without_deleted = current_sessions;
        sessions_without_deleted.remove(id);

        // Collect tree messages outside of lock (does I/O)
        let tree_to_delete = if let Some(tip_id) = tip_id_to_delete {
            Some(
                self.collect_tree_messages(&tip_id)
                    .await
                    .with_context(|| format!("Failed to traverse messages for session {id}"))?,
            )
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
pub fn agent_state_root() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("Failed to determine home directory")?;
    Ok(base_dirs.home_dir().join(".kraai"))
}

/// Get the data directory for the application
pub fn get_data_dir() -> Result<PathBuf> {
    Ok(agent_state_root()?.join("data"))
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
pub async fn init() -> Result<(
    Arc<FileMessageStore>,
    Arc<FileSessionStore>,
    Arc<FileScriptExecutionStore>,
    Arc<FileContextStateStore>,
)> {
    let data_dir = get_data_dir()?;
    fs::create_dir_all(&data_dir)
        .await
        .with_context(|| format!("Failed to create data directory: {:?}", data_dir))?;

    let message_store = Arc::new(FileMessageStore::new(&data_dir));
    let session_store = Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));
    let execution_store = Arc::new(FileScriptExecutionStore::new(&data_dir));
    let context_state_store = Arc::new(FileContextStateStore::new(&data_dir));

    session_store.load().await?;

    // Clean up any orphaned messages (e.g., from manually deleted sessions)
    session_store.cleanup_orphans().await?;

    Ok((
        message_store,
        session_store,
        execution_store,
        context_state_store,
    ))
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "persistence tests use direct assertions for fixture and failure-path setup"
)]
mod tests {
    use super::*;
    use kraai_types::{ChatRole, MessageStatus};
    use std::future::Future;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn session_temp_write_paths_are_unique_and_adjacent_to_destination() {
        let path = PathBuf::from("/tmp/kraai-data/sessions.json");

        let first = temp_write_path(&path);
        let second = temp_write_path(&path);

        assert_ne!(first, second);
        assert_eq!(first.parent(), path.parent());
        assert_eq!(second.parent(), path.parent());
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".sessions.json.")
        );
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".tmp")
        );
        assert!(
            second
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".sessions.json.")
        );
        assert!(
            second
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".tmp")
        );
    }

    #[tokio::test]
    async fn atomic_write_cleans_temp_file_after_replace_failure() {
        let data_dir = test_dir("atomic-write-cleanup");
        fs::create_dir_all(&data_dir).await.unwrap();
        let destination = data_dir.join("destination");
        fs::create_dir(&destination).await.unwrap();

        let error = atomic_write(&destination, b"contents").await.unwrap_err();

        assert!(error.to_string().contains("Failed to rename temp file"));
        let mut entries = fs::read_dir(&data_dir).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(names, vec![String::from("destination")]);
        let _ = fs::remove_dir_all(&data_dir).await;
    }

    #[tokio::test]
    async fn atomic_write_replaces_file_after_syncing_contents() {
        let data_dir = test_dir("atomic-write-success");
        let destination = data_dir.join("state.json");

        atomic_write(&destination, b"durable").await.unwrap();

        assert_eq!(fs::read(&destination).await.unwrap(), b"durable");
        let _ = fs::remove_dir_all(&data_dir).await;
    }

    fn test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("agent-persistence-{name}-{nanos}-{}", Ulid::new()))
    }

    async fn with_test_store<T, F, Fut>(name: &str, f: F) -> T
    where
        F: FnOnce(Arc<FileMessageStore>, Arc<FileSessionStore>, PathBuf) -> Fut,
        Fut: Future<Output = T>,
    {
        let data_dir = test_dir(name);
        fs::create_dir_all(&data_dir).await.unwrap();
        let message_store = Arc::new(FileMessageStore::new(&data_dir));
        let session_store = Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));
        let result = f(message_store, session_store, data_dir.clone()).await;
        let _ = fs::remove_dir_all(&data_dir).await;
        result
    }

    fn session(id: &str, tip_id: Option<&MessageId>, updated_at: u64) -> SessionMeta {
        SessionMeta {
            id: id.to_string(),
            tip_id: tip_id.cloned(),
            workspace_dir: PathBuf::from("/tmp/workspace"),
            created_at: updated_at.saturating_sub(1),
            updated_at,
            title: Some(format!("session-{id}")),
            selected_profile_id: None,
        }
    }

    fn message(id: &str, parent_id: Option<&MessageId>, content: &str) -> Message {
        Message {
            id: MessageId::new(id),
            parent_id: parent_id.cloned(),
            role: ChatRole::Assistant,
            content: content.to_string(),
            status: MessageStatus::Complete,
            agent_profile_id: None,
            generation: None,
        }
    }

    #[tokio::test]
    async fn save_failure_does_not_mutate_in_memory_sessions() {
        let data_dir = test_dir("save-failure");
        fs::create_dir_all(&data_dir).await.unwrap();

        let blocking_file = data_dir.join("not-a-directory");
        fs::write(&blocking_file, "x").await.unwrap();

        let message_store = Arc::new(FileMessageStore::new(&data_dir));
        let session_store = FileSessionStore::new(&blocking_file, message_store);

        let err = session_store
            .save(&session("broken", None, 1))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Failed to create directory"));
        assert!(session_store.list().await.unwrap().is_empty());

        let _ = fs::remove_dir_all(&data_dir).await;
    }

    #[tokio::test]
    async fn message_store_rejects_ids_that_could_escape_storage() {
        with_test_store(
            "unsafe-message-id",
            |message_store, _session_store, data_dir| async move {
                for raw in [
                    "../sessions",
                    "/tmp/kraai-message",
                    r"..\sessions",
                    "C:escape",
                ] {
                    let id = MessageId(Arc::from(raw));
                    let unsafe_message = Message {
                        id: id.clone(),
                        parent_id: None,
                        role: ChatRole::Assistant,
                        content: String::from("unsafe"),
                        status: MessageStatus::Complete,
                        agent_profile_id: None,
                        generation: None,
                    };

                    assert!(message_store.save(&unsafe_message).await.is_err());
                    assert!(message_store.get(&id).await.is_err());
                    assert!(message_store.exists(&id).await.is_err());
                    assert!(message_store.delete(&id).await.is_err());
                }

                assert!(!data_dir.join("sessions.json").exists());
            },
        )
        .await;
    }

    #[tokio::test]
    async fn concurrent_save_and_delete_preserve_unrelated_sessions() {
        with_test_store(
            "concurrent-save-delete",
            |message_store, session_store, _| async move {
                let base_message = message("shared-root", None, "root");
                message_store.save(&base_message).await.unwrap();

                session_store
                    .save(&session("keep", Some(&base_message.id), 2))
                    .await
                    .unwrap();
                session_store
                    .save(&session("drop", Some(&base_message.id), 1))
                    .await
                    .unwrap();

                let save_task = {
                    let session_store = session_store.clone();
                    tokio::spawn(async move {
                        session_store
                            .save(&session("new", Some(&MessageId::new("shared-root")), 3))
                            .await
                            .unwrap();
                    })
                };

                let delete_task = {
                    let session_store = session_store.clone();
                    tokio::spawn(async move {
                        session_store.delete("drop").await.unwrap();
                    })
                };

                save_task.await.unwrap();
                delete_task.await.unwrap();

                let ids: HashSet<_> = session_store
                    .list()
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|session| session.id)
                    .collect();

                assert_eq!(
                    ids,
                    HashSet::from([String::from("keep"), String::from("new")])
                );
            },
        )
        .await;
    }

    #[tokio::test]
    async fn deleting_session_removes_only_unique_messages() {
        with_test_store(
            "delete-unique-messages",
            |message_store, session_store, _| async move {
                let root = message("root", None, "root");
                let shared = message("shared", Some(&root.id), "shared");
                let a_tip = message("a-tip", Some(&shared.id), "a");
                let b_tip = message("b-tip", Some(&shared.id), "b");

                for msg in [&root, &shared, &a_tip, &b_tip] {
                    message_store.save(msg).await.unwrap();
                }

                session_store
                    .save(&session("a", Some(&a_tip.id), 2))
                    .await
                    .unwrap();
                session_store
                    .save(&session("b", Some(&b_tip.id), 1))
                    .await
                    .unwrap();

                session_store.delete("a").await.unwrap();

                assert!(!message_store.exists(&a_tip.id).await.unwrap());
                assert!(message_store.exists(&b_tip.id).await.unwrap());
                assert!(message_store.exists(&shared.id).await.unwrap());
                assert!(message_store.exists(&root.id).await.unwrap());
            },
        )
        .await;
    }

    #[tokio::test]
    async fn cyclic_message_graphs_return_corruption_errors() {
        with_test_store(
            "cyclic-message-graphs",
            |message_store, session_store, _| async move {
                let self_cycle_id = MessageId::new("self-cycle");
                message_store
                    .save(&message(
                        self_cycle_id.as_str(),
                        Some(&self_cycle_id),
                        "self",
                    ))
                    .await
                    .unwrap();
                let error = session_store
                    .collect_tree_messages(&self_cycle_id)
                    .await
                    .unwrap_err();
                assert!(error.to_string().contains("self-cycle"));

                let first_id = MessageId::new("cycle-a");
                let second_id = MessageId::new("cycle-b");
                message_store
                    .save(&message("cycle-a", Some(&second_id), "a"))
                    .await
                    .unwrap();
                message_store
                    .save(&message("cycle-b", Some(&first_id), "b"))
                    .await
                    .unwrap();
                let error = session_store
                    .collect_tree_messages(&first_id)
                    .await
                    .unwrap_err();
                assert!(error.to_string().contains("cycle-a"));
            },
        )
        .await;
    }

    #[tokio::test]
    async fn valid_deep_message_graph_still_traverses() {
        with_test_store(
            "deep-message-graph",
            |message_store, session_store, _| async move {
                let mut parent = None;
                for index in 0..256 {
                    let current = message(&format!("deep-{index}"), parent.as_ref(), "item");
                    message_store.save(&current).await.unwrap();
                    parent = Some(current.id);
                }

                let tree = session_store
                    .collect_tree_messages(parent.as_ref().unwrap())
                    .await
                    .unwrap();

                assert_eq!(tree.len(), 256);
            },
        )
        .await;
    }

    #[tokio::test]
    async fn list_sorts_sessions_by_updated_at_descending() {
        with_test_store(
            "list-order",
            |_message_store, session_store, _| async move {
                session_store.save(&session("old", None, 1)).await.unwrap();
                session_store.save(&session("new", None, 10)).await.unwrap();
                session_store.save(&session("mid", None, 5)).await.unwrap();

                let ordered_ids: Vec<_> = session_store
                    .list()
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|session| session.id)
                    .collect();

                assert_eq!(ordered_ids, vec!["new", "mid", "old"]);
            },
        )
        .await;
    }

    #[tokio::test]
    async fn delete_surfaces_orphan_cleanup_failures() {
        with_test_store(
            "delete-orphan-failure",
            |message_store, session_store, data_dir| async move {
                let orphan = message("orphan", None, "orphan");
                message_store.save(&orphan).await.unwrap();
                session_store
                    .save(&session("drop", Some(&orphan.id), 1))
                    .await
                    .unwrap();

                let orphan_path = data_dir
                    .join("messages")
                    .join(format!("{}.json", orphan.id));
                fs::remove_file(&orphan_path).await.unwrap();
                fs::create_dir(&orphan_path).await.unwrap();

                let err = session_store.delete("drop").await.unwrap_err();
                assert!(
                    err.to_string()
                        .contains("Failed to delete orphaned messages after session removal")
                );

                fs::remove_dir_all(&orphan_path).await.unwrap();
            },
        )
        .await;
    }
}
