use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::eyre::{Context, Result, eyre};
use kraai_types::MessageId;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};

use crate::MessageStore;
use crate::atomic_file::{AtomicWriteOutcome, atomic_write_with_outcome};
use crate::commit::complete_commit;
use crate::keyed_locks::KeyedLocks;

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

/// File-based session store
pub struct FileSessionStore {
    state: Arc<SessionState>,
    write_guard: Arc<Mutex<()>>,
    message_mutations: KeyedLocks<MessageId>,
}

struct SessionState {
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
            state: Arc::new(SessionState {
                sessions: RwLock::new(HashMap::new()),
                sessions_path,
                message_store,
            }),
            write_guard: Arc::default(),
            message_mutations: KeyedLocks::default(),
        }
    }

    pub(crate) async fn load(&self) -> Result<()> {
        self.state.load().await
    }

    pub(crate) async fn cleanup_orphans(&self) -> Result<usize> {
        self.state.cleanup_orphans().await
    }

    async fn commit_sessions(
        &self,
        guard: OwnedMutexGuard<()>,
        next_sessions: HashMap<String, SessionMeta>,
        deleted_tree: Option<HashSet<MessageId>>,
    ) -> Result<()> {
        let state = self.state.clone();
        complete_commit(
            guard,
            async move {
                let outcome =
                    SessionState::persist_sessions(&next_sessions, &state.sessions_path).await?;
                state.publish_sessions(next_sessions, outcome).await?;
                if let Some(tree) = deleted_tree {
                    state
                        .gc_orphaned_messages(tree, state.message_store.as_ref())
                        .await?;
                }
                Ok(())
            },
            "Session commit task failed",
        )
        .await
    }

    async fn update_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
        required_message: Option<&MessageId>,
    ) -> Result<bool> {
        let guard = self.write_guard.clone().lock_owned().await;

        let sessions = self.state.sessions.read().await;
        let Some(current) = sessions.get(&session.id) else {
            return Ok(false);
        };
        if current.tip_id.as_ref() != expected_tip {
            return Ok(false);
        }
        let mut next_sessions = sessions.clone();
        drop(sessions);
        if let Some(message_id) = required_message
            && !self.state.message_store.exists(message_id).await?
        {
            return Err(eyre!(
                "Cannot link missing message {message_id} to session {}",
                session.id
            ));
        }
        next_sessions.insert(session.id.clone(), session.clone());

        self.commit_sessions(guard, next_sessions, None)
            .await
            .map(|()| true)
    }
}

impl SessionState {
    /// Load sessions from disk (should be called on startup)
    async fn load(&self) -> Result<()> {
        if !self.sessions_path.exists() {
            return Ok(());
        }

        let content = fs::read_to_string(&self.sessions_path)
            .await
            .with_context(|| format!("Failed to read sessions file: {:?}", self.sessions_path))?;

        let sessions: HashMap<String, SessionMeta> =
            serde_json::from_str(&content).with_context(|| "Failed to parse sessions file")?;

        let previous = std::mem::replace(&mut *self.sessions.write().await, sessions);
        drop(previous);

        Ok(())
    }

    /// Persist sessions to disk (internal version that takes sessions map)
    async fn persist_sessions(
        sessions: &HashMap<String, SessionMeta>,
        path: &Path,
    ) -> Result<AtomicWriteOutcome> {
        let content = serde_json::to_string_pretty(sessions)
            .with_context(|| "Failed to serialize sessions")?;

        atomic_write_with_outcome(path, content.as_bytes()).await
    }

    async fn publish_sessions(
        &self,
        next_sessions: HashMap<String, SessionMeta>,
        outcome: AtomicWriteOutcome,
    ) -> Result<()> {
        let previous = std::mem::replace(&mut *self.sessions.write().await, next_sessions);
        drop(previous);
        outcome.into_result()
    }

    /// Collect all message IDs in a session's tree (from tip to root)
    async fn collect_tree_messages(&self, tip_id: &MessageId) -> Result<HashSet<MessageId>> {
        self.collect_tree_messages_until(tip_id, &HashSet::new())
            .await
    }

    async fn collect_tree_messages_until(
        &self,
        tip_id: &MessageId,
        known: &HashSet<MessageId>,
    ) -> Result<HashSet<MessageId>> {
        let mut messages = HashSet::new();
        let mut current = Some(tip_id.clone());

        while let Some(id) = current {
            if known.contains(&id) {
                break;
            }
            if !messages.insert(id.clone()) {
                return Err(eyre!(
                    "Corrupt message parent graph: cycle repeats message {id}"
                ));
            }
            current = self.message_store.read_parent_id(&id).await?;
        }

        Ok(messages)
    }

    /// Collect all message IDs referenced by all sessions
    async fn collect_all_referenced_messages(&self) -> Result<HashSet<MessageId>> {
        let session_tips: Vec<_> = self
            .sessions
            .read()
            .await
            .values()
            .filter_map(|session| {
                session
                    .tip_id
                    .as_ref()
                    .map(|tip| (session.id.clone(), tip.clone()))
            })
            .collect();
        let mut all_messages = HashSet::new();

        for (session_id, tip_id) in session_tips {
            let tree = self
                .collect_tree_messages_until(&tip_id, &all_messages)
                .await
                .with_context(|| format!("Failed to traverse messages for session {session_id}"))?;
            all_messages.extend(tree);
        }

        Ok(all_messages)
    }

    /// Garbage collect orphaned messages after deleting a session
    async fn gc_orphaned_messages(
        &self,
        deleted_tree: HashSet<MessageId>,
        message_store: &dyn MessageStore,
    ) -> Result<()> {
        let still_referenced = self.collect_all_referenced_messages().await?;

        let mut deleted_messages: Vec<_> = deleted_tree.into_iter().collect();
        deleted_messages.sort();

        let mut errors = Vec::new();
        for msg_id in deleted_messages {
            if !still_referenced.contains(&msg_id)
                && let Err(e) = message_store.delete(&msg_id).await
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
        let mut list: Vec<_> = self.state.sessions.read().await.values().cloned().collect();
        list.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        Ok(list)
    }

    async fn list_ids(&self) -> Result<HashSet<String>> {
        Ok(self
            .state
            .sessions
            .read()
            .await
            .values()
            .map(|session| session.id.clone())
            .collect())
    }

    async fn get(&self, id: &str) -> Result<Option<SessionMeta>> {
        let sessions = self.state.sessions.read().await;
        Ok(sessions.get(id).cloned())
    }

    async fn save(&self, session: &SessionMeta) -> Result<()> {
        let guard = self.write_guard.clone().lock_owned().await;

        let mut next_sessions = self.state.sessions.read().await.clone();
        next_sessions.insert(session.id.clone(), session.clone());

        self.commit_sessions(guard, next_sessions, None).await
    }

    async fn save_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
    ) -> Result<bool> {
        self.update_if_tip_matches(session, expected_tip, None)
            .await
    }

    async fn link_message_if_tip_matches(
        &self,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
    ) -> Result<bool> {
        self.update_if_tip_matches(session, expected_tip, session.tip_id.as_ref())
            .await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let guard = self.write_guard.clone().lock_owned().await;

        let current_sessions = self.state.sessions.read().await.clone();
        let tip_id_to_delete = current_sessions.get(id).and_then(|s| s.tip_id.clone());
        let mut sessions_without_deleted = current_sessions;
        sessions_without_deleted.remove(id);

        let tree_to_delete = if let Some(tip_id) = tip_id_to_delete {
            Some(
                self.state
                    .collect_tree_messages(&tip_id)
                    .await
                    .with_context(|| format!("Failed to traverse messages for session {id}"))?,
            )
        } else {
            None
        };

        self.commit_sessions(guard, sessions_without_deleted, tree_to_delete)
            .await
    }

    async fn lock_message_mutation(&self, id: &MessageId) -> OwnedMutexGuard<()> {
        self.message_mutations.lock(id).await
    }

    async fn delete_message_if_unreferenced(
        &self,
        id: &MessageId,
        message_store: Arc<dyn MessageStore>,
    ) -> Result<()> {
        let guard = self.write_guard.clone().lock_owned().await;
        let state = self.state.clone();
        let messages = HashSet::from([id.clone()]);
        complete_commit(
            guard,
            async move {
                state
                    .gc_orphaned_messages(messages, message_store.as_ref())
                    .await
            },
            "Message cleanup task failed",
        )
        .await
    }
}

impl SessionState {
    /// Clean up orphaned messages (messages on disk not referenced by any session)
    async fn cleanup_orphans(&self) -> Result<usize> {
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

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "persistence tests use direct assertions for fixture and failure-path setup"
)]
mod tests;
