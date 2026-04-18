use std::collections::{BTreeMap, HashMap};

use kraai_runtime::{
    AgentProfileSummary, AgentProfilesState, Model, ProviderDefinition, Session,
    SessionContextUsage as RuntimeSessionContextUsage, SettingsDocument,
};
use kraai_types::{Message, MessageId, RiskLevel};

use super::auth::ProviderAuthStatus;

pub(super) const DEFAULT_AGENT_PROFILE_ID: &str = "plan-code";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartupOptions {
    pub ci: bool,
    pub auto_approve: bool,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub agent_profile_id: Option<String>,
    pub message: Option<String>,
}

pub(super) fn default_agent_profiles() -> Vec<AgentProfileSummary> {
    vec![
        AgentProfileSummary {
            id: String::from("plan-code"),
            display_name: String::from("Plan Code"),
            description: String::from("Read-only planning agent"),
            tools: vec![
                String::from("list_files"),
                String::from("search_files"),
                String::from("read_files"),
            ],
            default_risk_level: RiskLevel::ReadOnlyWorkspace,
            source: kraai_runtime::AgentProfileSource::BuiltIn,
        },
        AgentProfileSummary {
            id: String::from("build-code"),
            display_name: String::from("Build Code"),
            description: String::from("Implementation agent"),
            tools: vec![
                String::from("list_files"),
                String::from("search_files"),
                String::from("read_files"),
                String::from("edit_file"),
            ],
            default_risk_level: RiskLevel::UndoableWorkspaceWrite,
            source: kraai_runtime::AgentProfileSource::BuiltIn,
        },
    ]
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
pub(super) enum ToolApprovalAction {
    Allow,
    Reject,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolPhase {
    Idle,
    Deciding,
    ExecutingBatch,
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
pub(super) struct PendingTool {
    pub(super) call_id: String,
    pub(super) tool_id: String,
    pub(super) args: String,
    pub(super) description: String,
    pub(super) risk_level: String,
    pub(super) reasons: Vec<String>,
    pub(super) approved: Option<bool>,
    pub(super) queue_order: u64,
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
pub(super) struct OptimisticToolMessage {
    pub(super) local_id: String,
    pub(super) content: String,
}

#[derive(Clone, Debug)]
pub(super) struct PendingSubmit {
    pub(super) session_id: Option<String>,
    pub(super) message: String,
    pub(super) model_id: String,
    pub(super) provider_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ChatCellPosition {
    pub(super) line: usize,
    pub(super) column: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ChatSelection {
    pub(super) anchor: ChatCellPosition,
    pub(super) focus: ChatCellPosition,
}

impl ChatSelection {
    pub(super) fn normalized(self) -> (ChatCellPosition, ChatCellPosition) {
        if self.anchor.line < self.focus.line
            || (self.anchor.line == self.focus.line && self.anchor.column <= self.focus.column)
        {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }
}

pub(super) enum RuntimeRequest {
    ListModels,
    ListAgentProfiles {
        session_id: String,
    },
    ListProviderDefinitions,
    GetSettings,
    GetOpenAiCodexAuthStatus,
    StartOpenAiCodexBrowserLogin,
    StartOpenAiCodexDeviceCodeLogin,
    CancelOpenAiCodexLogin,
    LogoutOpenAiCodexAuth,
    CreateSession,
    SetSessionProfile {
        session_id: String,
        profile_id: String,
    },
    SendMessage {
        session_id: String,
        message: String,
        model_id: String,
        provider_id: String,
        auto_approve: bool,
    },
    SaveSettings {
        settings: SettingsDocument,
    },
    GetChatHistory {
        session_id: String,
    },
    GetSessionContextUsage {
        session_id: String,
    },
    GetCurrentTip {
        session_id: String,
    },
    UndoLastUserMessage {
        session_id: String,
    },
    GetPendingTools {
        session_id: String,
    },
    LoadSession {
        session_id: String,
    },
    ListSessions,
    DeleteSession {
        session_id: String,
    },
    ApproveTool {
        session_id: String,
        call_id: String,
    },
    DenyTool {
        session_id: String,
        call_id: String,
    },
    CancelStream {
        session_id: String,
    },
    ContinueSession {
        session_id: String,
    },
    ExecuteApprovedTools {
        session_id: String,
    },
}

pub(super) enum RuntimeResponse {
    Models(Result<HashMap<String, Vec<Model>>, String>),
    AgentProfiles {
        session_id: String,
        result: Result<AgentProfilesState, String>,
    },
    ProviderDefinitions(Result<Vec<ProviderDefinition>, String>),
    Settings(Result<SettingsDocument, String>),
    OpenAiCodexAuthStatus(Result<ProviderAuthStatus, String>),
    StartOpenAiCodexBrowserLogin(Result<ProviderAuthStatus, String>),
    StartOpenAiCodexDeviceCodeLogin(Result<ProviderAuthStatus, String>),
    CancelOpenAiCodexLogin(Result<ProviderAuthStatus, String>),
    LogoutOpenAiCodexAuth(Result<ProviderAuthStatus, String>),
    CreateSession(Result<String, String>),
    SetSessionProfile {
        profile_id: String,
        result: Result<(), String>,
    },
    SendMessage(Result<(), String>),
    SaveSettings(Result<(), String>),
    ChatHistory {
        session_id: String,
        result: Result<BTreeMap<MessageId, Message>, String>,
    },
    SessionContextUsage {
        session_id: String,
        result: Result<Option<RuntimeSessionContextUsage>, String>,
    },
    CurrentTip {
        session_id: String,
        result: Result<Option<String>, String>,
    },
    UndoLastUserMessage {
        session_id: String,
        result: Result<Option<String>, String>,
    },
    PendingTools {
        session_id: String,
        result: Result<Vec<kraai_runtime::PendingToolInfo>, String>,
    },
    LoadSession {
        session_id: String,
        result: Result<bool, String>,
    },
    Sessions(Result<Vec<Session>, String>),
    DeleteSession {
        session_id: String,
        result: Result<(), String>,
    },
    ApproveTool {
        call_id: String,
        result: Result<(), String>,
    },
    DenyTool {
        call_id: String,
        result: Result<(), String>,
    },
    CancelStream(Result<bool, String>),
    ContinueSession(Result<(), String>),
    ExecuteApprovedTools(Result<(), String>),
}
