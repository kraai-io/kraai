use std::collections::{BTreeMap, HashMap, HashSet};

use kraai_runtime::{
    AgentProfileCatalog, Model, ProviderDefinition, Session, SessionSnapshot, SettingsDocument,
};
use kraai_types::{ImageAttachment, MessageContent, MessageId, SessionId, TokenUsage};

use super::auth::ProviderAuthStatus;

type RuntimeResult<T> = Result<T, kraai_runtime::RuntimeError>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartupOptions {
    pub agent_profile_id: Option<String>,
    pub message: Option<String>,
}

pub(super) enum SubmissionSource {
    Composer { queued: bool },
    Pending,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UiMode {
    Chat,
    Executions,
    AgentMenu,
    ModelMenu,
    ModelOptionsMenu,
    ProvidersMenu,
    Mcp,
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
pub(super) struct PendingMessage {
    pub(super) session_id: SessionId,
    pub(super) message: MessageContent,
    pub(super) local_id: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct PendingSubmit {
    pub(super) creation_id: u64,
    pub(super) session_id: Option<SessionId>,
    pub(super) message: MessageContent,
    pub(super) model_id: String,
    pub(super) provider_id: String,
    pub(super) options: kraai_types::ModelOptionValues,
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
    FinishStartupSync,
    ListModels,
    GetAgentProfileCatalog,
    ListProviderDefinitions,
    GetSettings,
    GetProviderAuthStatuses,
    GetMcpAuthStatuses,
    StartMcpLogin {
        server: String,
        request_id: u64,
    },
    CancelMcpLogin {
        server: String,
    },
    LogoutMcp {
        server: String,
    },
    StartProviderLogin {
        provider_type: String,
        method: kraai_runtime::LoginMethod,
    },
    CancelProviderLogin {
        provider_type: String,
    },
    LogoutProvider {
        provider_type: String,
    },
    CreateSession {
        creation_id: u64,
        profile_id: Option<String>,
    },
    SetSessionProfile {
        session_id: SessionId,
        profile_id: String,
    },
    SetSessionModel {
        session_id: SessionId,
        save_id: u64,
        selection: kraai_types::ModelSelection,
    },
    SendMessage {
        session_id: SessionId,
        message: MessageContent,
        model_id: String,
        provider_id: String,
        options: kraai_types::ModelOptionValues,
    },
    PasteImage {
        request_id: u64,
    },
    SaveSettings {
        settings: SettingsDocument,
    },
    GetHistoryPage {
        session_id: SessionId,
        from: MessageId,
    },
    GetSessionSnapshot {
        session_id: SessionId,
    },
    UndoLastUserMessage {
        session_id: SessionId,
    },
    LoadSession {
        load_id: u64,
        session_id: SessionId,
    },
    ListSessions,
    ListUserInputHistory {
        limit: usize,
    },
    DeleteSession {
        session_id: SessionId,
    },
    ApproveScript {
        session_id: SessionId,
        execution_id: String,
    },
    DenyScript {
        session_id: SessionId,
        execution_id: String,
    },
    CancelStream {
        session_id: SessionId,
    },
    ContinueSession {
        session_id: SessionId,
        model_id: String,
        provider_id: String,
        options: kraai_types::ModelOptionValues,
    },
}

pub(super) enum RuntimeResponse {
    StartupSyncComplete,
    Models(RuntimeResult<HashMap<String, Vec<Model>>>),
    AgentProfileCatalog(RuntimeResult<AgentProfileCatalog>),
    ProviderDefinitions(RuntimeResult<Vec<ProviderDefinition>>),
    Settings(RuntimeResult<SettingsDocument>),
    ProviderAuthStatuses(RuntimeResult<Vec<ProviderAuthStatus>>),
    McpAuthStatuses(RuntimeResult<Vec<kraai_runtime::McpAuthStatus>>),
    StartMcpLogin {
        server: String,
        request_id: u64,
        result: RuntimeResult<kraai_runtime::McpAuthStatus>,
    },
    McpAuthStatus(RuntimeResult<kraai_runtime::McpAuthStatus>),
    ProviderAuthStatus(RuntimeResult<ProviderAuthStatus>),
    CreateSession {
        creation_id: u64,
        result: RuntimeResult<SessionId>,
    },
    SetSessionProfile {
        session_id: SessionId,
        profile_id: String,
        result: RuntimeResult<()>,
    },
    SetSessionModel {
        session_id: SessionId,
        save_id: u64,
        result: RuntimeResult<()>,
    },
    SendMessage {
        session_id: SessionId,
        result: RuntimeResult<kraai_runtime::SubmitMessageOutcome>,
    },
    PasteImage {
        request_id: u64,
        result: RuntimeResult<Option<ImageAttachment>>,
    },
    SaveSettings(RuntimeResult<()>),
    HistoryPage {
        session_id: SessionId,
        result: RuntimeResult<kraai_runtime::HistoryPage>,
    },
    SessionSnapshot {
        session_id: SessionId,
        model_save_id: u64,
        result: Box<RuntimeResult<SessionSnapshot>>,
    },
    UndoLastUserMessage {
        session_id: SessionId,
        result: RuntimeResult<Option<MessageContent>>,
    },
    LoadSession {
        load_id: u64,
        session_id: SessionId,
        result: RuntimeResult<bool>,
    },
    Sessions(RuntimeResult<Vec<Session>>),
    UserInputHistory(RuntimeResult<Vec<String>>),
    DeleteSession {
        session_id: SessionId,
        result: RuntimeResult<()>,
    },
    ApproveScript {
        session_id: SessionId,
        execution_id: String,
        result: RuntimeResult<()>,
    },
    DenyScript {
        session_id: SessionId,
        execution_id: String,
        result: RuntimeResult<()>,
    },
    CancelStream(RuntimeResult<bool>),
    ContinueSession(RuntimeResult<kraai_runtime::ContinueSessionOutcome>),
}
