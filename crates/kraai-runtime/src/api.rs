use std::collections::BTreeMap;

use kraai_execution_types::{DomainError, DomainErrorKind};
use kraai_mcp::McpAuthStatus;
use kraai_persistence::SessionMeta;
pub use kraai_provider_core::{
    LoginMethod, PendingBrowserLogin, PendingDeviceCodeLogin, ProviderAuthStatus,
    ProviderLoginState,
};
use kraai_types::{
    AgentProfileSummary, AgentProfileWarning, Message, MessageId, SessionId, TokenUsage,
};
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeErrorKind {
    InvalidArgument,
    NotFound,
    Conflict,
    Validation,
    Unavailable,
    Internal,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldViolation {
    pub field: String,
    pub message: String,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeError {
    pub kind: RuntimeErrorKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<FieldViolation>,
}

impl RuntimeError {
    pub fn new(kind: RuntimeErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            violations: Vec::new(),
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(RuntimeErrorKind::InvalidArgument, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(RuntimeErrorKind::NotFound, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(RuntimeErrorKind::Conflict, message)
    }

    pub fn validation(violations: Vec<FieldViolation>) -> Self {
        Self {
            kind: RuntimeErrorKind::Validation,
            message: String::from("Settings validation failed"),
            violations,
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(RuntimeErrorKind::Unavailable, message)
    }

    pub(crate) fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(RuntimeErrorKind::Internal, error.to_string())
    }
}

impl From<DomainError> for RuntimeError {
    fn from(error: DomainError) -> Self {
        let kind = match error.kind() {
            DomainErrorKind::InvalidArgument => RuntimeErrorKind::InvalidArgument,
            DomainErrorKind::NotFound => RuntimeErrorKind::NotFound,
            DomainErrorKind::Conflict => RuntimeErrorKind::Conflict,
        };
        Self::new(kind, error.message())
    }
}

impl From<kraai_agent::AgentError> for RuntimeError {
    fn from(error: kraai_agent::AgentError) -> Self {
        match error {
            kraai_agent::AgentError::Domain(error) => error.into(),
            kraai_agent::AgentError::Internal(report) => Self::internal(format!("{report:#}")),
        }
    }
}

impl From<color_eyre::Report> for RuntimeError {
    fn from(report: color_eyre::Report) -> Self {
        kraai_agent::AgentError::from(report).into()
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RuntimeError {}

pub type RuntimeResult<T> = std::result::Result<T, RuntimeError>;

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum SubmitMessageOutcome {
    Started { message_id: String },
    Queued { position: usize },
}

/// Raise on any incompatible change.
pub const PROTOCOL_VERSION: u32 = 1;

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeStartupState {
    Starting,
    Ready { protocol_version: u32 },
    Failed(String),
}

impl RuntimeStartupState {
    #[must_use]
    pub fn ready() -> Self {
        Self::Ready {
            protocol_version: PROTOCOL_VERSION,
        }
    }
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub max_context: Option<usize>,
    #[serde(default)]
    pub options: Vec<kraai_types::ModelOptionDefinition>,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionContextUsage {
    pub provider_id: String,
    pub model_id: String,
    pub max_context: Option<usize>,
    pub usage: TokenUsage,
}

impl SessionContextUsage {
    pub fn used_context_tokens(&self) -> usize {
        self.usage.used_context_tokens()
    }
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub tip_id: Option<String>,
    pub workspace_dir: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub title: Option<String>,
    pub selected_profile_id: Option<String>,
    pub selected_model: Option<kraai_types::ModelSelection>,
    pub profile_locked: bool,
    pub waiting_for_approval: bool,
    pub is_streaming: bool,
    pub is_running: bool,
}

impl Session {
    pub(crate) fn from_session_meta(meta: SessionMeta) -> Self {
        Session {
            id: meta.id,
            tip_id: meta.tip_id.map(|id| id.to_string()),
            workspace_dir: meta.workspace_dir.display().to_string(),
            created_at: meta.created_at,
            updated_at: meta.updated_at,
            title: meta.title,
            selected_profile_id: meta.selected_profile_id,
            selected_model: meta.selected_model,
            profile_locked: false,
            waiting_for_approval: false,
            is_streaming: false,
            is_running: false,
        }
    }
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScriptOutputLine {
    pub stream: kraai_execution_types::ScriptOutputStream,
    pub text: String,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingScriptInfo {
    pub execution_id: String,
    pub call_id: String,
    pub source: String,
    pub requested_capabilities: Vec<String>,
    pub capability_additions: Vec<String>,
    pub timeout_millis: u64,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub workspace_dir: String,
    pub applies_next_chat: bool,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfileCatalog {
    pub workspace_dir: String,
    pub profiles: Vec<AgentProfileSummary>,
    pub warnings: Vec<AgentProfileWarning>,
    pub default_profile_id: String,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub workspace_dir: Option<String>,
    pub profile_id: Option<String>,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActivity {
    Idle,
    Streaming,
    AwaitingApproval,
    ExecutingScript,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Cancelled,
    Failed { error: RuntimeError },
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryPage {
    pub messages: BTreeMap<MessageId, Message>,
    pub older_history: Option<MessageId>,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub requests: BTreeMap<MessageId, kraai_types::RequestUsage>,
    pub turn_timer: crate::TurnTimer,
    /// After installing the snapshot, discard events through this sequence for this session only;
    /// unrelated session and service events still need handling.
    pub event_sequence: u64,
    pub session: Session,
    pub history: BTreeMap<MessageId, Message>,
    pub older_history: Option<MessageId>,
    pub context_usage: Option<SessionContextUsage>,
    pub pending_script: Option<PendingScriptInfo>,
    pub profiles: kraai_types::AgentProfilesState,
    pub activity: SessionActivity,
    pub queued_messages: usize,
    /// How the last turn this process ran for the session ended. `None` once a message is
    /// submitted or a stream starts.
    pub turn_outcome: Option<TurnOutcome>,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeEvent {
    pub sequence: u64,
    pub event: Event,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
    TurnCompleted {
        session_id: SessionId,
    },
    ScriptPrepared {
        session_id: SessionId,
        execution_id: String,
        call_id: String,
        source: String,
    },
    ScriptStarted {
        session_id: SessionId,
        call_id: String,
        timeout_millis: u64,
    },
    /// Sent at most ten times a second while output arrives; each one replaces the one before.
    ScriptOutputTail {
        session_id: SessionId,
        call_id: String,
        lines: Vec<ScriptOutputLine>,
    },
    RequestUsageUpdated {
        session_id: SessionId,
        request: Box<kraai_types::RequestUsage>,
    },
    TurnTimingChanged {
        session_id: SessionId,
        timer: crate::TurnTimer,
    },
    ConfigLoaded,
    ModelsUpdated,
    ServiceError {
        error: RuntimeError,
    },
    SessionError {
        session_id: SessionId,
        error: RuntimeError,
    },
    StreamStart {
        session_id: SessionId,
        message_id: String,
    },
    ItemStarted {
        session_id: SessionId,
        message_id: String,
        item_index: usize,
        item: kraai_types::AssistantItem,
    },
    ItemDelta {
        session_id: SessionId,
        message_id: String,
        item_index: usize,
        delta: String,
    },
    /// For display only; it is not kept in history.
    ReasoningDelta {
        session_id: SessionId,
        message_id: String,
        item_id: String,
        delta: String,
    },
    StreamComplete {
        session_id: SessionId,
        message_id: String,
    },
    StreamError {
        session_id: SessionId,
        message_id: String,
        error: String,
    },
    StreamCancelled {
        session_id: SessionId,
        message_id: String,
    },
    ProviderRetryScheduled {
        session_id: SessionId,
        provider_id: String,
        model_id: String,
        operation: String,
        retry_number: u32,
        delay_seconds: u64,
        reason: String,
    },
    /// The partial content was discarded and the same request is being issued again.
    StreamRestarted {
        session_id: SessionId,
        message_id: String,
        reason: String,
    },

    ScriptApprovalRequested {
        session_id: SessionId,
        script: PendingScriptInfo,
    },
    ScriptResultReady {
        session_id: SessionId,
        execution_id: String,
        call_id: String,
        output: kraai_types::MessageContent,
        outcome: kraai_execution_types::ScriptExecutionOutcome,
    },
    ContextStateChanged {
        session_id: SessionId,
        notifications: Vec<String>,
    },
    ContinuationFailed {
        session_id: SessionId,
        error: String,
    },

    SessionStateChanged {
        session_id: SessionId,
        activity: SessionActivity,
        is_running: bool,
    },
    MessageUpserted {
        session_id: SessionId,
        message: Box<Message>,
    },
    MessagesRemoved {
        session_id: SessionId,
        message_ids: Vec<MessageId>,
    },
    /// The session changed outside this runtime; reload it from a snapshot.
    HistoryUpdated {
        session_id: SessionId,
    },
    ProviderAuthUpdated {
        status: ProviderAuthStatus,
    },
    McpAuthUpdated {
        status: McpAuthStatus,
    },
}

impl Event {
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            Self::TurnCompleted { session_id }
            | Self::ScriptPrepared { session_id, .. }
            | Self::ScriptStarted { session_id, .. }
            | Self::ScriptOutputTail { session_id, .. }
            | Self::RequestUsageUpdated { session_id, .. }
            | Self::TurnTimingChanged { session_id, .. }
            | Self::SessionError { session_id, .. }
            | Self::StreamStart { session_id, .. }
            | Self::ItemStarted { session_id, .. }
            | Self::ItemDelta { session_id, .. }
            | Self::ReasoningDelta { session_id, .. }
            | Self::StreamComplete { session_id, .. }
            | Self::StreamError { session_id, .. }
            | Self::StreamCancelled { session_id, .. }
            | Self::ProviderRetryScheduled { session_id, .. }
            | Self::StreamRestarted { session_id, .. }
            | Self::ScriptApprovalRequested { session_id, .. }
            | Self::ScriptResultReady { session_id, .. }
            | Self::ContextStateChanged { session_id, .. }
            | Self::ContinuationFailed { session_id, .. }
            | Self::SessionStateChanged { session_id, .. }
            | Self::MessageUpserted { session_id, .. }
            | Self::MessagesRemoved { session_id, .. }
            | Self::HistoryUpdated { session_id } => Some(session_id),
            Self::ConfigLoaded
            | Self::ModelsUpdated
            | Self::ServiceError { .. }
            | Self::McpAuthUpdated { .. }
            | Self::ProviderAuthUpdated { .. } => None,
        }
    }
}

pub trait EventCallback: Send + Sync {
    fn on_event(&self, event: Event);
}

impl<F> EventCallback for F
where
    F: Fn(Event) + Send + Sync,
{
    fn on_event(&self, event: Event) {
        self(event)
    }
}
