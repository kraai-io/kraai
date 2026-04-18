use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use kraai_persistence::{MessageStore, SessionMeta, SessionStore};
use kraai_provider_core::{Model, ProviderManager, ProviderManagerConfig, ProviderRegistry};
use kraai_tool_core::{
    PreparedToolCall, ToolManager,
    toon_parser::{self, ParseFailure, ParseFailureKind},
};
use kraai_types::{
    AgentProfilesState, CallId, ChatMessage, ChatRole, Message, MessageGeneration, MessageId,
    MessageStatus, ModelId, ProviderId, RiskLevel, TokenUsage, ToolCall, ToolCallAssessment,
    ToolId, ToolResult, ToolStateSnapshot,
};
use tokio::sync::RwLock;
use ulid::Ulid;

use crate::profiles::{AgentProfile, ResolvedProfiles, resolve_profiles};
use crate::tool_state::{
    refresh_and_render_system_prompt as render_tool_state_prompt, resolve_snapshot_from_history,
};

mod prompts;
mod sessions;
mod streaming;
mod tool_calls;

#[cfg(test)]
mod tests;

const DEFAULT_AGENT_PROFILE_ID: &str = "plan-code";
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

#[derive(Clone)]
pub struct PendingToolCall {
    pub call: ToolCall,
    pub source_message_id: MessageId,
    pub prepared: PreparedToolCall,
    pub description: String,
    pub assessment: ToolCallAssessment,
    pub config: kraai_types::ToolCallGlobalConfig,
    pub tool_state_snapshot: ToolStateSnapshot,
    pub status: PermissionStatus,
    pub queue_order: u64,
}

impl std::fmt::Debug for PendingToolCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingToolCall")
            .field("call", &self.call)
            .field("source_message_id", &self.source_message_id)
            .field("description", &self.description)
            .field("assessment", &self.assessment)
            .field("config", &self.config)
            .field("tool_state_snapshot", &self.tool_state_snapshot)
            .field("status", &self.status)
            .field("queue_order", &self.queue_order)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PermissionStatus {
    Pending,
    Approved,
    Denied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingToolInfo {
    pub call_id: CallId,
    pub tool_id: ToolId,
    pub args: serde_json::Value,
    pub description: String,
    pub risk_level: RiskLevel,
    pub reasons: Vec<String>,
    pub approved: Option<bool>,
    pub queue_order: u64,
}

#[derive(Clone)]
pub enum ToolExecutionPayload {
    Approved {
        prepared: PreparedToolCall,
        config: kraai_types::ToolCallGlobalConfig,
        tool_state_snapshot: ToolStateSnapshot,
    },
    Denied,
}

#[derive(Clone)]
pub struct ToolExecutionRequest {
    pub call_id: CallId,
    pub tool_id: ToolId,
    pub source_message_id: MessageId,
    pub payload: ToolExecutionPayload,
}

#[derive(Clone, Debug)]
pub struct PendingStreamRequest {
    pub message_id: MessageId,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub provider_messages: Vec<ChatMessage>,
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
    active_tool_config: kraai_types::ToolCallGlobalConfig,
    pending_tool_config: Option<kraai_types::ToolCallGlobalConfig>,
    pending_tool_calls: HashMap<CallId, PendingToolCall>,
    in_flight_tool_calls: HashMap<MessageId, usize>,
    next_tool_queue_order: u64,
    last_model: Option<ModelId>,
    last_provider: Option<ProviderId>,
    active_turn_profile: Option<AgentProfile>,
    active_turn_auto_approve: bool,
    active_turn_tool_state_snapshot: Option<ToolStateSnapshot>,
}

impl SessionRuntimeState {
    fn new(workspace_dir: PathBuf) -> Self {
        Self {
            active_tool_config: kraai_types::ToolCallGlobalConfig { workspace_dir },
            pending_tool_config: None,
            pending_tool_calls: HashMap::new(),
            in_flight_tool_calls: HashMap::new(),
            next_tool_queue_order: 0,
            last_model: None,
            last_provider: None,
            active_turn_profile: None,
            active_turn_auto_approve: false,
            active_turn_tool_state_snapshot: None,
        }
    }

    fn effective_workspace_dir(&self) -> PathBuf {
        self.pending_tool_config
            .as_ref()
            .unwrap_or(&self.active_tool_config)
            .workspace_dir
            .clone()
    }

    fn promote_pending_tool_config(&mut self) {
        if let Some(config) = self.pending_tool_config.take() {
            self.active_tool_config = config;
        }
    }
}

#[derive(Clone, Debug)]
struct StreamingMessageState {
    session_id: String,
    previous_tip: Option<MessageId>,
    message: Message,
}

pub struct AgentManager {
    providers: ProviderManager,
    tools: ToolManager,
    default_workspace_dir: PathBuf,
    message_store: Arc<dyn MessageStore>,
    session_store: Arc<dyn SessionStore>,
    session_states: HashMap<String, SessionRuntimeState>,
    last_used_profile_id: Option<String>,
    /// Messages currently being streamed (not yet persisted).
    streaming_messages: RwLock<HashMap<MessageId, StreamingMessageState>>,
}

#[derive(Clone, Debug)]
pub struct DetectedToolCall {
    pub call_id: CallId,
    pub tool_id: String,
    pub source_message_id: MessageId,
    pub description: String,
    pub assessment: ToolCallAssessment,
    pub requires_confirmation: bool,
    pub queue_order: u64,
}
