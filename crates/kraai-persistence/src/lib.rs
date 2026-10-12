#![forbid(unsafe_code)]

use color_eyre::eyre::{ContextCompat, Result};
use directories::BaseDirs;
use std::path::PathBuf;

mod commit;
mod compaction;
mod context;
mod database;
mod executions;
mod history;
mod images;
mod keyed_locks;
mod leases;
mod message_references;
mod messages;
mod preferences;
mod queue;
mod repository;
mod sessions;
mod stream_deltas;
mod turns;
mod usage;
pub use compaction::{CompactionCheckpoint, SqliteCompactionStore};
pub use database::Database as SqliteDatabase;
pub use history::{HistoryCursor, history_by_id};
pub use images::FileImageStore;
pub use leases::{SessionObservation, TURN_LEASE_DURATION};
pub use messages::{ConversationSnapshot, LinkRequest, MessageStore, SqliteMessageStore};
pub use preferences::{WorkspacePreferences, WorkspacePreferencesStore};
pub use queue::{QueuedEntry, QueuedMessage};
pub use repository::Persistence;
pub use sessions::{SessionMeta, SessionStore, SqliteSessionStore};
pub use stream_deltas::StreamDelta;
pub use usage::{RequestUsageStore, SqliteRequestUsageStore};

pub use context::{
    ContextStateDocument, ContextStateStore, FileContextSnapshot, SqliteContextStateStore,
};
pub use executions::{
    NewScriptExecution, PersistedScriptOutput, ScriptExecutionCompletion, ScriptExecutionRecord,
    ScriptExecutionStore, SqliteScriptExecutionStore,
};
pub use turns::{
    AppendMessageRequest, AppendedMessage, AppendedMessages, ConversationStore,
    IdempotentAppendOutcome, NewMessage,
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
mod tests;
