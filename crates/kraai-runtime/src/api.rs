use std::collections::BTreeMap;

use kraai_persistence::SessionMeta;
use kraai_types::{
    AgentProfileSummary, AgentProfileWarning, DomainError, DomainErrorKind, Message, MessageId,
    TokenUsage,
};
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldViolation {
    pub field: String,
    pub message: String,
}

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

    pub(crate) fn from_report(error: color_eyre::Report) -> Self {
        let Some(domain_error) = error.downcast_ref::<DomainError>() else {
            return Self::internal(error);
        };
        let kind = match domain_error.kind() {
            DomainErrorKind::InvalidArgument => RuntimeErrorKind::InvalidArgument,
            DomainErrorKind::NotFound => RuntimeErrorKind::NotFound,
            DomainErrorKind::Conflict => RuntimeErrorKind::Conflict,
        };
        Self::new(kind, domain_error.message())
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RuntimeError {}

pub type RuntimeResult<T> = std::result::Result<T, RuntimeError>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum SubmitMessageOutcome {
    Started { message_id: String },
    Queued { position: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinueSessionOutcome {
    Started,
    NothingToContinue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeStartupState {
    Starting,
    Ready,
    Failed(String),
}

/// Model information
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub max_context: Option<usize>,
}

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

/// Session information
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub tip_id: Option<String>,
    pub workspace_dir: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub title: Option<String>,
    pub selected_profile_id: Option<String>,
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
            profile_locked: false,
            waiting_for_approval: false,
            is_streaming: false,
            is_running: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingScriptInfo {
    pub execution_id: String,
    pub source: String,
    pub requested_capabilities: Vec<String>,
    pub capability_additions: Vec<String>,
    pub timeout_millis: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub workspace_dir: String,
    pub applies_next_chat: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingBrowserLogin {
    pub auth_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDeviceCodeLogin {
    pub verification_url: String,
    pub user_code: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenAiCodexLoginState {
    SignedOut,
    BrowserPending(PendingBrowserLogin),
    DeviceCodePending(PendingDeviceCodeLogin),
    Authenticated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiCodexAuthStatus {
    pub state: OpenAiCodexLoginState,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub account_id: Option<String>,
    pub last_refresh_unix: Option<u64>,
    pub error: Option<String>,
}

/// Streaming events sent from the runtime to clients
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfileCatalog {
    pub workspace_dir: String,
    pub profiles: Vec<AgentProfileSummary>,
    pub warnings: Vec<AgentProfileWarning>,
    pub default_profile_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub workspace_dir: Option<String>,
    pub profile_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActivity {
    Idle,
    Streaming,
    AwaitingApproval,
    ExecutingScript,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub requests: BTreeMap<MessageId, kraai_types::RequestUsage>,
    pub turn_timer: crate::TurnTimer,
    /// The snapshot contains this session's state represented by events through this global
    /// sequence. After installing it, discard covered events for this session only; unrelated
    /// session and service events still need to be handled.
    pub event_sequence: u64,
    pub session: Session,
    pub history: BTreeMap<MessageId, Message>,
    pub context_usage: Option<SessionContextUsage>,
    pub pending_script: Option<PendingScriptInfo>,
    pub profiles: kraai_types::AgentProfilesState,
    pub activity: SessionActivity,
    pub queued_messages: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeEvent {
    pub sequence: u64,
    pub event: Event,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
    RequestUsageUpdated {
        session_id: String,
        request: Box<kraai_types::RequestUsage>,
    },
    TurnTimingChanged {
        session_id: String,
        timer: crate::TurnTimer,
    },
    /// Configuration loaded successfully
    ConfigLoaded,
    /// Failure in runtime-wide background work or hosting.
    ServiceError {
        error: RuntimeError,
    },
    /// Failure in background work scoped to one session.
    SessionError {
        session_id: String,
        error: RuntimeError,
    },
    // Streaming events
    /// Stream started for a message
    StreamStart {
        session_id: String,
        message_id: String,
    },
    /// Chunk received for a streaming message
    StreamChunk {
        session_id: String,
        message_id: String,
        chunk: String,
    },
    /// Stream completed for a message
    StreamComplete {
        session_id: String,
        message_id: String,
    },
    /// Stream error for a message
    StreamError {
        session_id: String,
        message_id: String,
        error: String,
    },
    /// Stream cancelled by the user
    StreamCancelled {
        session_id: String,
        message_id: String,
    },
    ProviderRetryScheduled {
        session_id: String,
        provider_id: String,
        model_id: String,
        operation: String,
        retry_number: u32,
        delay_seconds: u64,
        reason: String,
    },

    ScriptApprovalRequested {
        session_id: String,
        script: PendingScriptInfo,
    },
    ScriptResultReady {
        session_id: String,
        execution_id: String,
        status: String,
    },
    ContextStateChanged {
        session_id: String,
        notifications: Vec<String>,
    },
    ContinuationFailed {
        session_id: String,
        error: String,
    },

    // History events
    /// Chat history was updated
    HistoryUpdated {
        session_id: String,
    },
    OpenAiCodexAuthUpdated {
        status: OpenAiCodexAuthStatus,
    },
}

impl Event {
    /// Returns the session this event concerns, if any.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::RequestUsageUpdated { session_id, .. }
            | Self::TurnTimingChanged { session_id, .. }
            | Self::SessionError { session_id, .. }
            | Self::StreamStart { session_id, .. }
            | Self::StreamChunk { session_id, .. }
            | Self::StreamComplete { session_id, .. }
            | Self::StreamError { session_id, .. }
            | Self::StreamCancelled { session_id, .. }
            | Self::ProviderRetryScheduled { session_id, .. }
            | Self::ScriptApprovalRequested { session_id, .. }
            | Self::ScriptResultReady { session_id, .. }
            | Self::ContextStateChanged { session_id, .. }
            | Self::ContinuationFailed { session_id, .. }
            | Self::HistoryUpdated { session_id } => Some(session_id),
            Self::ConfigLoaded
            | Self::ServiceError { .. }
            | Self::OpenAiCodexAuthUpdated { .. } => None,
        }
    }
}

/// Optional callback adapter for receiving runtime events.
///
/// The primary runtime API is subscription-based via `RuntimeHandle::subscribe`.
/// This trait remains available for local adapters and tests that want a
/// callback-style sink.
pub trait EventCallback: Send + Sync {
    /// Called when an event occurs
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
