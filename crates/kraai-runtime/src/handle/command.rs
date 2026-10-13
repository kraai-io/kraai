use std::collections::HashMap;

use kraai_provider_core::ProviderDefinition;
use kraai_types::{
    MessageId, ModelId, ModelOptionValues, ProviderId, ScriptExecutionId, SessionId,
};
use tokio::sync::oneshot;

use crate::{
    AgentProfileCatalog, AgentProfilesState, ContinueSessionOutcome, CreateSessionRequest, Model,
    RuntimeResult, Session, SessionContextUsage, SessionSnapshot, SettingsDocument,
    SubmitMessageOutcome, WorkspaceState,
};

/// Internal commands sent to the runtime
pub(crate) enum Command {
    GetMcpAuthStatuses {
        response: oneshot::Sender<RuntimeResult<Vec<crate::McpAuthStatus>>>,
    },
    StartMcpLogin {
        server: String,
        response: oneshot::Sender<RuntimeResult<crate::McpAuthStatus>>,
    },
    CancelMcpLogin {
        server: String,
        response: oneshot::Sender<RuntimeResult<crate::McpAuthStatus>>,
    },
    LogoutMcp {
        server: String,
        response: oneshot::Sender<RuntimeResult<crate::McpAuthStatus>>,
    },
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
        session_id: SessionId,
        response: oneshot::Sender<RuntimeResult<AgentProfilesState>>,
    },
    GetAgentProfileCatalog {
        workspace_dir: Option<String>,
        response: oneshot::Sender<RuntimeResult<AgentProfileCatalog>>,
    },
    SetSessionProfile {
        session_id: SessionId,
        profile_id: String,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    SaveSettings {
        settings: SettingsDocument,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    CreateSession {
        request: CreateSessionRequest,
        response: oneshot::Sender<RuntimeResult<SessionId>>,
    },
    SendMessage {
        session_id: SessionId,
        message: kraai_types::MessageContent,
        model_id: ModelId,
        provider_id: ProviderId,
        options: ModelOptionValues,
        response: oneshot::Sender<RuntimeResult<SubmitMessageOutcome>>,
    },
    ImportImage {
        bytes: Vec<u8>,
        response: oneshot::Sender<RuntimeResult<kraai_types::ImageAttachment>>,
    },
    ReadImage {
        image: kraai_types::ImageAttachment,
        response: oneshot::Sender<RuntimeResult<Vec<u8>>>,
    },
    LoadConfig,
    LoadSession {
        session_id: SessionId,
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
        session_id: SessionId,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    GetWorkspaceState {
        session_id: SessionId,
        response: oneshot::Sender<RuntimeResult<Option<WorkspaceState>>>,
    },
    SetWorkspaceDir {
        session_id: SessionId,
        workspace_dir: String,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    GetTip {
        session_id: SessionId,
        response: oneshot::Sender<RuntimeResult<Option<String>>>,
    },
    UndoLastUserMessage {
        session_id: SessionId,
        response: oneshot::Sender<RuntimeResult<Option<kraai_types::MessageContent>>>,
    },
    GetHistoryPage {
        session_id: SessionId,
        from: Option<MessageId>,
        limit: usize,
        response: oneshot::Sender<RuntimeResult<crate::HistoryPage>>,
    },
    GetSessionSnapshot {
        session_id: SessionId,
        response: oneshot::Sender<RuntimeResult<SessionSnapshot>>,
    },
    GetSessionContextUsage {
        session_id: SessionId,
        response: oneshot::Sender<RuntimeResult<Option<SessionContextUsage>>>,
    },
    GetSessionModel {
        session_id: SessionId,
        response: oneshot::Sender<RuntimeResult<Option<kraai_types::ModelSelection>>>,
    },
    SetSessionModel {
        session_id: SessionId,
        selection: kraai_types::ModelSelection,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    SetSessionMcpServers {
        session_id: SessionId,
        config: kraai_mcp::McpConfig,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    GetPendingScript {
        session_id: SessionId,
        response: oneshot::Sender<RuntimeResult<Option<crate::PendingScriptInfo>>>,
    },
    ApproveScript {
        session_id: SessionId,
        execution_id: ScriptExecutionId,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    DenyScript {
        session_id: SessionId,
        execution_id: ScriptExecutionId,
        response: oneshot::Sender<RuntimeResult<()>>,
    },
    CancelStream {
        session_id: SessionId,
        response: oneshot::Sender<RuntimeResult<bool>>,
    },
    CancelTurn {
        session_id: SessionId,
        response: oneshot::Sender<RuntimeResult<bool>>,
    },
    ContinueSession {
        session_id: SessionId,
        model_id: ModelId,
        provider_id: ProviderId,
        options: ModelOptionValues,
        wait_for_preparation: bool,
        response: oneshot::Sender<RuntimeResult<ContinueSessionOutcome>>,
    },
    GetProviderAuthStatuses {
        response: oneshot::Sender<RuntimeResult<Vec<crate::ProviderAuthStatus>>>,
    },
    StartProviderLogin {
        provider_type: String,
        method: crate::LoginMethod,
        response: oneshot::Sender<RuntimeResult<crate::ProviderAuthStatus>>,
    },
    CancelProviderLogin {
        provider_type: String,
        response: oneshot::Sender<RuntimeResult<crate::ProviderAuthStatus>>,
    },
    LogoutProvider {
        provider_type: String,
        response: oneshot::Sender<RuntimeResult<crate::ProviderAuthStatus>>,
    },
    Shutdown {
        response: Option<oneshot::Sender<RuntimeResult<()>>>,
    },
}
