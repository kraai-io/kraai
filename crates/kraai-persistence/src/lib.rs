#![forbid(unsafe_code)]

use color_eyre::eyre::{Context, ContextCompat, Result};
use directories::BaseDirs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

mod atomic_file;
mod commit;
mod compaction;
mod context;
mod executions;
mod keyed_locks;
mod messages;
mod preferences;
mod sessions;
mod turns;
mod usage;
pub use atomic_file::atomic_write;
pub(crate) use atomic_file::sync_parent_directory;
pub use compaction::{CompactionCheckpoint, FileCompactionStore};
pub use messages::{FileMessageStore, MessageStore};
pub use preferences::{WorkspacePreferences, WorkspacePreferencesStore};
pub use sessions::{FileSessionStore, SessionMeta, SessionStore};
pub use usage::{FileRequestUsageStore, RequestUsageStore};

pub use context::{ContextStateStore, FileContextStateStore};
pub use executions::{
    FileScriptExecutionStore, NewScriptExecution, PersistedScriptOutput, ScriptExecutionCompletion,
    ScriptExecutionRecord, ScriptExecutionStore,
};
pub use turns::{
    AppendMessageRequest, AppendedMessage, ConversationStore, IdempotentAppendOutcome,
};

/// Get the data directory for the application
pub fn agent_state_root() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("Failed to determine home directory")?;
    Ok(base_dirs.home_dir().join(".kraai"))
}

/// Get the data directory for the application
pub fn get_data_dir() -> Result<PathBuf> {
    Ok(agent_state_root()?.join("data"))
}

/// Initialize the persistence layer
pub async fn init() -> Result<(
    Arc<FileMessageStore>,
    Arc<FileSessionStore>,
    Arc<FileScriptExecutionStore>,
    Arc<FileContextStateStore>,
)> {
    init_at(&get_data_dir()?).await
}

pub async fn init_at(
    data_dir: &Path,
) -> Result<(
    Arc<FileMessageStore>,
    Arc<FileSessionStore>,
    Arc<FileScriptExecutionStore>,
    Arc<FileContextStateStore>,
)> {
    fs::create_dir_all(data_dir)
        .await
        .with_context(|| format!("Failed to create data directory: {:?}", data_dir))?;

    let message_store = Arc::new(FileMessageStore::new(data_dir));
    let session_store = Arc::new(FileSessionStore::new(data_dir, message_store.clone()));
    let execution_store = Arc::new(FileScriptExecutionStore::new(data_dir));
    let context_state_store = Arc::new(FileContextStateStore::new(data_dir));

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
mod test_support;
