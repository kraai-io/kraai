use std::collections::{BTreeMap, HashMap};

use kraai_provider_core::ProviderDefinition;
use kraai_types::{MessageId, ModelId, ProviderId, ScriptExecutionId};
use tokio::sync::oneshot;

use crate::{
    AgentProfileCatalog, AgentProfilesState, ContinueSessionOutcome, CreateSessionRequest, Model,
    OpenAiCodexAuthStatus, RuntimeResult, Session, SessionContextUsage, SessionSnapshot,
    SettingsDocument, SubmitMessageOutcome, WorkspaceState,
};

/// Internal commands sent to the runtime
pub(crate) enum Command {
    ListModels {
        response: oneshot::Sender<RuntimeResult<HashMap<String, Vec<Model>>>>,
    },
    ListProviderDefinitions {
        response: oneshot::Sender<RuntimeResult<Vec<ProviderDefinition>>>,
    },
    GetSettings {
        response: oneshot::Sender<RuntimeResult<SettingsDocument>>,
    },
    ListAgentProfiles {
        session_id: String,
        response: oneshot::Sender<RuntimeResult<AgentProfilesState>>,
    },
    GetAgentProfileCatalog {
        workspace_dir: Option<String>,
        response: oneshot::Sender<RuntimeResult<AgentProfileCatalog>>,
    },
    SetSessionProfile {
        session_id: String,
        profile_id: String,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    SaveSettings {
        settings: SettingsDocument,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    CreateSession {
        request: CreateSessionRequest,
        response: oneshot::Sender<RuntimeResult<String>>,
    },
    SendMessage {
        session_id: String,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
        response: oneshot::Sender<RuntimeResult<SubmitMessageOutcome>>,
    },
    StartQueuedMessages {
        session_id: String,
    },
    LoadConfig,
    LoadSession {
        session_id: String,
        response: oneshot::Sender<RuntimeResult<bool>>,
    },
    ListSessions {
        response: oneshot::Sender<RuntimeResult<Vec<Session>>>,
    },
    ListUserInputHistory {
        limit: usize,
        response: oneshot::Sender<RuntimeResult<Vec<String>>>,
    },
    DeleteSession {
        session_id: String,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    GetWorkspaceState {
        session_id: String,
        response: oneshot::Sender<RuntimeResult<Option<WorkspaceState>>>,
    },
    SetWorkspaceDir {
        session_id: String,
        workspace_dir: String,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    GetTip {
        session_id: String,
        response: oneshot::Sender<RuntimeResult<Option<String>>>,
    },
    UndoLastUserMessage {
        session_id: String,
        response: oneshot::Sender<RuntimeResult<Option<String>>>,
    },
    GetChatHistory {
        session_id: String,
        response: oneshot::Sender<RuntimeResult<BTreeMap<MessageId, kraai_types::Message>>>,
    },
    GetSessionSnapshot {
        session_id: String,
        response: oneshot::Sender<RuntimeResult<SessionSnapshot>>,
    },
    GetSessionContextUsage {
        session_id: String,
        response: oneshot::Sender<RuntimeResult<Option<SessionContextUsage>>>,
    },
    GetPendingScript {
        session_id: String,
        response: oneshot::Sender<RuntimeResult<Option<crate::PendingScriptInfo>>>,
    },
    ApproveScript {
        session_id: String,
        execution_id: ScriptExecutionId,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    DenyScript {
        session_id: String,
        execution_id: ScriptExecutionId,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    CancelStream {
        session_id: String,
        response: oneshot::Sender<RuntimeResult<bool>>,
    },
    ContinueSession {
        session_id: String,
        response: oneshot::Sender<RuntimeResult<ContinueSessionOutcome>>,
    },
    GetOpenAiCodexAuthStatus {
        response: oneshot::Sender<RuntimeResult<OpenAiCodexAuthStatus>>,
    },
    StartOpenAiCodexBrowserLogin {
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    StartOpenAiCodexDeviceCodeLogin {
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    CancelOpenAiCodexLogin {
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    LogoutOpenAiCodexAuth {
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    Shutdown {
        response: Option<oneshot::Sender<RuntimeResult<()>>>,
    },
}
