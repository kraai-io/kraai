#![forbid(unsafe_code)]

use color_eyre::eyre::{ContextCompat, Result};
use directories::BaseDirs;
use std::path::PathBuf;

mod commit;
mod compaction;
mod context;
mod executions;
mod images;
mod keyed_locks;
mod messages;
mod preferences;
mod repository;
mod sessions;
mod turns;
mod usage;
pub use compaction::{CompactionCheckpoint, FileCompactionStore};
pub use images::FileImageStore;
pub use messages::{FileMessageStore, MessageStore};
pub use preferences::{WorkspacePreferences, WorkspacePreferencesStore};
pub use repository::Persistence;
pub use sessions::{FileSessionStore, SessionMeta, SessionStore};
pub use usage::{FileRequestUsageStore, RequestUsageStore};

pub use context::{
    ContextStateDocument, ContextStateStore, FileContextSnapshot, FileContextStateStore,
};
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

#[cfg(test)]
mod test_support;
