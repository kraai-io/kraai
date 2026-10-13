use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex as StdMutex, MutexGuard, PoisonError, RwLock as StdRwLock, RwLockReadGuard,
    RwLockWriteGuard,
};

use color_eyre::eyre::{Result, eyre};
use kraai_persistence::{
    AppendMessageRequest, ContextStateStore, ConversationStore, HistoryCursor, MessageStore,
    SessionMeta, SessionStore,
};
use kraai_provider_core::{
    Model, ProviderManager, ProviderManagerConfig, ProviderRegistry, ProviderRequest,
    ScriptToolDefinition,
};
use kraai_types::{
    AgentProfilesState, AssistantItem, AssistantPhase, ChatRole, ConversationItem, Message,
    MessageGeneration, MessageId, MessageStatus, ModelId, ModelOptionValues, ProviderId,
    ScriptProfileSnapshot, SessionId, StreamId, TokenUsage, ToolCallId,
};
use tokio::sync::{Mutex, RwLock};
use ulid::Ulid;

use crate::error::{AgentError, AgentResult};
use crate::profiles::{AgentProfile, ResolvedProfiles, resolve_profiles};

mod ancestry;
mod history_changes;
mod intercepted;
mod mcp;
mod model_context;
mod prompts;
mod sessions;
mod snapshot;
mod stream_requests;
mod streaming;
mod usage;

pub use history_changes::{HistoryChange, HistoryObserver, StreamedItem};
pub use snapshot::{SessionSnapshotData, SessionSnapshotReader};

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic_in_result_fn,
    reason = "integration-style manager tests use direct assertions and fixtures"
)]
mod tests;

const DEFAULT_AGENT_PROFILE_ID: &str = "coding";
const AGENTS_MD_FILE_NAME: &str = "AGENTS.md";
const SESSION_TITLE_MAX_CHARS: usize = 60;

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn read<T>(lock: &StdRwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &StdRwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

fn title_from_user_prompt(prompt: &str) -> Option<String> {
    let title: String = prompt
        .split_whitespace()
        .flat_map(|word| std::iter::once(' ').chain(word.chars()))
        .skip(1)
        .take(SESSION_TITLE_MAX_CHARS)
        .collect();
    if title.is_empty() { None } else { Some(title) }
}

fn current_unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs()
}

#[derive(Clone, Debug)]
pub struct PendingStreamRequest {
    pub message_id: MessageId,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub provider_request: ProviderRequest,
    pub context_notifications: Vec<String>,
    /// Summarizes older history; runs before the request when `compact_first` is set, and
    /// after the provider reports that the request overflowed the model's context window.
    pub context_compaction: Option<crate::ContextCompaction>,
    pub compact_first: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptTurnContext {
    pub workspace_dir: PathBuf,
    pub profile: ScriptProfileSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelledStreamResult {
    pub session_id: SessionId,
    pub message_id: MessageId,
    pub persisted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContextUsage {
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub max_context: Option<usize>,
    pub usage: TokenUsage,
}

#[derive(Clone)]
struct SessionRuntimeState {
    active_workspace_dir: PathBuf,
    pending_workspace_dir: Option<PathBuf>,
    last_model: Option<ModelId>,
    last_provider: Option<ProviderId>,
    last_options: ModelOptionValues,
    active_turn_profile: Option<Arc<AgentProfile>>,
}

impl SessionRuntimeState {
    fn new(workspace_dir: PathBuf) -> Self {
        Self {
            active_workspace_dir: workspace_dir,
            pending_workspace_dir: None,
            last_model: None,
            last_provider: None,
            last_options: ModelOptionValues::new(),
            active_turn_profile: None,
        }
    }

    fn effective_workspace_dir(&self) -> PathBuf {
        self.pending_workspace_dir
            .as_ref()
            .unwrap_or(&self.active_workspace_dir)
            .clone()
    }

    fn promote_pending_workspace_dir(&mut self) {
        if let Some(workspace_dir) = self.pending_workspace_dir.take() {
            self.active_workspace_dir = workspace_dir;
        }
    }
}

#[derive(Debug)]
struct StreamingMessageState {
    session_id: SessionId,
    previous_tip: Option<MessageId>,
    previous_title: Option<String>,
    message: Message,
    cancellation_result_id: Option<MessageId>,
    text_item_ids: HashMap<String, usize>,
    pending_deltas: Vec<kraai_persistence::StreamDelta>,
    first_pending_sequence: u64,
    sealed_deltas: usize,
    request_started_at: u64,
    unpriced_attempts: u32,
    reissued_attempts: u32,
    subscription: bool,
}

impl StreamingMessageState {
    fn record(&mut self, delta: kraai_persistence::StreamDelta) -> Result<()> {
        delta.apply(&mut self.message)?;
        self.queue_delta(delta);
        Ok(())
    }

    fn queue_delta(&mut self, delta: kraai_persistence::StreamDelta) {
        let unsealed = self.pending_deltas.len() > self.sealed_deltas;
        let unmerged = match self.pending_deltas.last_mut() {
            Some(last) if unsealed => last.coalesce(delta),
            _ => Some(delta),
        };
        self.pending_deltas.extend(unmerged);
    }
}

pub struct AgentManager {
    mcp: StdRwLock<Arc<kraai_mcp::McpManager>>,
    session_mcp: StdMutex<BTreeMap<SessionId, Arc<kraai_mcp::McpManager>>>,
    usage_store: Arc<dyn kraai_persistence::RequestUsageStore>,
    providers: StdRwLock<ProviderManager>,
    default_workspace_dir: PathBuf,
    storage_root: PathBuf,
    user_agents_path: Option<PathBuf>,
    persistence: kraai_persistence::Persistence,
    conversation_store: ConversationStore,
    message_store: Arc<dyn MessageStore>,
    session_store: Arc<dyn SessionStore>,
    context_state_store: Arc<dyn ContextStateStore>,
    session_states: StdMutex<HashMap<SessionId, SessionRuntimeState>>,
    last_used_profile_id: StdMutex<Option<String>>,
    /// Messages currently being streamed (not yet persisted).
    streaming_messages: RwLock<BTreeMap<MessageId, StreamingMessageState>>,
    streaming_persistence: Mutex<()>,
    history_observer: StdRwLock<Option<HistoryObserver>>,
}

impl AgentManager {
    pub fn mcp(&self) -> Arc<kraai_mcp::McpManager> {
        read(&self.mcp).clone()
    }

    pub fn set_mcp(&self, mcp: Arc<kraai_mcp::McpManager>) {
        *write(&self.mcp) = mcp;
    }

    pub fn image_store(&self) -> kraai_persistence::FileImageStore {
        self.persistence.images().as_ref().clone()
    }
}
