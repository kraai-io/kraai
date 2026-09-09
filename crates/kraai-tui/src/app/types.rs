use std::collections::{BTreeMap, HashMap, HashSet};

use kraai_runtime::{
    AgentProfileCatalog, Model, ProviderDefinition, Session, SessionSnapshot, SettingsDocument,
};
use kraai_types::{Message, MessageId, TokenUsage};

use super::auth::ProviderAuthStatus;

type RuntimeResult<T> = Result<T, kraai_runtime::RuntimeError>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartupOptions {
    pub ci: bool,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub agent_profile_id: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UiMode {
    Chat,
    AgentMenu,
    ModelMenu,
    ProvidersMenu,
    SessionsMenu,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProvidersView {
    List,
    Connect,
    Detail,
    Advanced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScriptApprovalAction {
    Allow,
    Reject,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScriptPhase {
    Idle,
    AwaitingApproval,
    Executing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SettingsFocus {
    ProviderList,
    ProviderForm,
    ModelList,
    ModelForm,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SettingsProviderField {
    Id,
    TypeId,
    Value(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SettingsModelField {
    Id,
    Value(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ActiveSettingsEditor {
    Provider(SettingsProviderField),
    Model(SettingsModelField),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProviderDetailAction {
    BrowserLogin,
    DeviceCodeLogin,
    CancelLogin,
    Logout,
    Advanced,
    RefreshModels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProvidersAdvancedFocus {
    ProviderFields,
    Models,
    ModelFields,
}

#[derive(Clone, Debug)]
pub(super) struct OptimisticMessage {
    pub(super) local_id: String,
    pub(super) content: String,
    pub(super) content_key: String,
    pub(super) occurrence: usize,
    pub(super) is_queued: bool,
}

#[derive(Clone, Debug)]
pub(super) struct PendingSubmit {
    pub(super) creation_id: u64,
    pub(super) session_id: Option<String>,
    pub(super) message: String,
    pub(super) model_id: String,
    pub(super) provider_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ExitUsageTotals {
    pub(super) completed_message_ids: HashSet<MessageId>,
    pub(super) counted_message_ids: HashSet<MessageId>,
    pub(super) usage_by_model: BTreeMap<UsageModelKey, TokenUsage>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct UsageModelKey {
    pub(super) provider_id: String,
    pub(super) model_id: String,
}

pub(super) enum RuntimeRequest {
    ListModels,
    GetAgentProfileCatalog,
    ListProviderDefinitions,
    GetSettings,
    GetOpenAiCodexAuthStatus,
    StartOpenAiCodexBrowserLogin,
    StartOpenAiCodexDeviceCodeLogin,
    CancelOpenAiCodexLogin,
    LogoutOpenAiCodexAuth,
    CreateSession {
        creation_id: u64,
        profile_id: Option<String>,
    },
    SetSessionProfile {
        session_id: String,
        profile_id: String,
    },
    SendMessage {
        session_id: String,
        message: String,
        model_id: String,
        provider_id: String,
    },
    SaveSettings {
        settings: SettingsDocument,
    },
    GetChatHistory {
        session_id: String,
    },
    GetSessionSnapshot {
        session_id: String,
    },
    GetCurrentTip {
        session_id: String,
    },
    UndoLastUserMessage {
        session_id: String,
    },
    LoadSession {
        session_id: String,
    },
    ListSessions,
    ListUserInputHistory {
        limit: usize,
    },
    DeleteSession {
        session_id: String,
    },
    ApproveScript {
        session_id: String,
        execution_id: String,
    },
    DenyScript {
        session_id: String,
        execution_id: String,
    },
    CancelStream {
        session_id: String,
    },
    ContinueSession {
        session_id: String,
    },
}

pub(super) enum RuntimeResponse {
    Models(RuntimeResult<HashMap<String, Vec<Model>>>),
    AgentProfileCatalog(RuntimeResult<AgentProfileCatalog>),
    ProviderDefinitions(RuntimeResult<Vec<ProviderDefinition>>),
    Settings(RuntimeResult<SettingsDocument>),
    OpenAiCodexAuthStatus(RuntimeResult<ProviderAuthStatus>),
    StartOpenAiCodexBrowserLogin(RuntimeResult<ProviderAuthStatus>),
    StartOpenAiCodexDeviceCodeLogin(RuntimeResult<ProviderAuthStatus>),
    CancelOpenAiCodexLogin(RuntimeResult<ProviderAuthStatus>),
    LogoutOpenAiCodexAuth(RuntimeResult<ProviderAuthStatus>),
    CreateSession {
        creation_id: u64,
        result: RuntimeResult<String>,
    },
    SetSessionProfile {
        session_id: String,
        profile_id: String,
        result: RuntimeResult<()>,
    },
    SendMessage(RuntimeResult<kraai_runtime::SubmitMessageOutcome>),
    SaveSettings(RuntimeResult<()>),
    ChatHistory {
        session_id: String,
        result: RuntimeResult<BTreeMap<MessageId, Message>>,
    },
    SessionSnapshot {
        session_id: String,
        result: Box<RuntimeResult<SessionSnapshot>>,
    },
    CurrentTip {
        session_id: String,
        result: RuntimeResult<Option<String>>,
    },
    UndoLastUserMessage {
        session_id: String,
        result: RuntimeResult<Option<String>>,
    },
    LoadSession {
        session_id: String,
        result: RuntimeResult<bool>,
    },
    Sessions(RuntimeResult<Vec<Session>>),
    UserInputHistory(RuntimeResult<Vec<String>>),
    DeleteSession {
        session_id: String,
        result: RuntimeResult<()>,
    },
    ApproveScript {
        session_id: String,
        execution_id: String,
        result: RuntimeResult<()>,
    },
    DenyScript {
        session_id: String,
        execution_id: String,
        result: RuntimeResult<()>,
    },
    CancelStream(RuntimeResult<bool>),
    ContinueSession(RuntimeResult<kraai_runtime::ContinueSessionOutcome>),
}
