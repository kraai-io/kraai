use persistence::SessionMeta;

/// Model information
#[derive(Clone, Debug)]
pub struct Model {
    pub id: String,
    pub name: String,
}

/// Session information
#[derive(Clone, Debug)]
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
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingToolInfo {
    pub call_id: String,
    pub tool_id: String,
    pub args: String,
    pub description: String,
    pub risk_level: String,
    pub reasons: Vec<String>,
    pub approved: Option<bool>,
    pub queue_order: u64,
}

#[derive(Clone, Debug)]
pub struct WorkspaceState {
    pub workspace_dir: String,
    pub applies_next_chat: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingBrowserLogin {
    pub auth_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingDeviceCodeLogin {
    pub verification_url: String,
    pub user_code: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenAiCodexLoginState {
    SignedOut,
    BrowserPending(PendingBrowserLogin),
    DeviceCodePending(PendingDeviceCodeLogin),
    Authenticated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAiCodexAuthStatus {
    pub state: OpenAiCodexLoginState,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub account_id: Option<String>,
    pub last_refresh_unix: Option<u64>,
    pub error: Option<String>,
}

/// Streaming events sent from the runtime to clients
#[derive(Clone, Debug)]
pub enum Event {
    /// Configuration loaded successfully
    ConfigLoaded,
    /// General error
    Error(String),
    /// Message completed (legacy)
    MessageComplete(String),

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

    // Tool events
    /// Tool call detected, awaiting permission
    ToolCallDetected {
        session_id: String,
        call_id: String,
        tool_id: String,
        args: String,
        description: String,
        risk_level: String,
        reasons: Vec<String>,
        queue_order: u64,
    },
    /// Tool execution result ready
    ToolResultReady {
        session_id: String,
        call_id: String,
        tool_id: String,
        success: bool,
        output: String,
        denied: bool,
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
