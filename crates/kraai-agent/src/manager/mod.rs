use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use kraai_persistence::{
    AppendMessageRequest, AppendedMessage, ContextStateStore, ConversationStore, MessageStore,
    SessionMeta, SessionStore,
};
use kraai_provider_core::{
    Model, ProviderManager, ProviderManagerConfig, ProviderRegistry, ProviderRequest,
    ScriptToolDefinition, ScriptToolTransport,
};
use kraai_types::{
    AgentProfilesState, AssistantItem, AssistantPhase, ChatRole, ConversationItem, Message,
    MessageGeneration, MessageId, MessageStatus, ModelId, ProviderId, ScriptProfileSnapshot,
    StreamId, TokenUsage, ToolCallId,
};
use tokio::sync::RwLock;
use ulid::Ulid;

use crate::profiles::{AgentProfile, ResolvedProfiles, resolve_profiles};

mod prompts;
mod sessions;
mod streaming;

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic_in_result_fn,
    reason = "integration-style manager tests use direct assertions and fixtures"
)]
mod tests;

const DEFAULT_AGENT_PROFILE_ID: &str = "plan";
const AGENTS_MD_FILE_NAME: &str = "AGENTS.md";
const SESSION_TITLE_MAX_CHARS: usize = 60;

fn title_from_user_prompt(prompt: &str) -> Option<String> {
    let normalized = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = normalized.chars().take(SESSION_TITLE_MAX_CHARS).collect();
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
    pub script_tool_transport: ScriptToolTransport,
    pub context_notifications: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptTurnContext {
    pub workspace_dir: PathBuf,
    pub profile: ScriptProfileSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelledStreamResult {
    pub session_id: String,
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
    active_turn_profile: Option<AgentProfile>,
}

impl SessionRuntimeState {
    fn new(workspace_dir: PathBuf) -> Self {
        Self {
            active_workspace_dir: workspace_dir,
            pending_workspace_dir: None,
            last_model: None,
            last_provider: None,
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

#[derive(Clone, Debug)]
struct StreamingMessageState {
    session_id: String,
    previous_tip: Option<MessageId>,
    previous_title: Option<String>,
    message: Message,
    text_item_ids: HashMap<String, usize>,
}

pub struct AgentManager {
    providers: ProviderManager,
    default_workspace_dir: PathBuf,
    conversation_store: ConversationStore,
    message_store: Arc<dyn MessageStore>,
    session_store: Arc<dyn SessionStore>,
    context_state_store: Arc<dyn ContextStateStore>,
    session_states: HashMap<String, SessionRuntimeState>,
    last_used_profile_id: Option<String>,
    /// Messages currently being streamed (not yet persisted).
    streaming_messages: RwLock<HashMap<MessageId, StreamingMessageState>>,
}
