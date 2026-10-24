use std::collections::HashMap;
use std::sync::Arc;

use kraai_execution_types::ScriptExecutionId;
use kraai_provider_core::ProviderDefinition;
use kraai_types::{MessageId, ModelId, ModelOptionValues, ProviderId, SessionId};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::RuntimeError;
use crate::{
    AgentProfileCatalog, AgentProfilesState, CreateSessionRequest, Model, RuntimeEvent,
    RuntimeResult, RuntimeStartupState, Session, SessionContextUsage, SessionSnapshot,
    SettingsDocument, SubmitMessageOutcome, WorkspaceState,
};

mod command;
mod events;
mod lifecycle;
#[cfg(test)]
mod tests;
pub(crate) use command::Command;
pub(crate) use events::RuntimeEventSender;
pub(crate) use lifecycle::RuntimeLifecycle;

/// Handle to the runtime for sending commands
///
/// This is cheaply cloneable and can be passed around to different parts
/// of the application.
#[derive(Clone)]
pub struct RuntimeHandle {
    pub(crate) command_tx: mpsc::Sender<Command>,
    pub(crate) event_tx: RuntimeEventSender,
    pub(crate) lifecycle: Option<Arc<RuntimeLifecycle>>,
    pub(crate) startup_rx: tokio::sync::watch::Receiver<RuntimeStartupState>,
}

#[expect(
    clippy::map_err_ignore,
    reason = "closed Tokio channels do not expose a useful source error"
)]
impl RuntimeHandle {
    fn command_channel_closed() -> RuntimeError {
        RuntimeError::unavailable("runtime command channel is closed")
    }

    fn response_channel_closed() -> RuntimeError {
        RuntimeError::unavailable("runtime response channel is closed")
    }

    async fn transport_error(&self, fallback: RuntimeError) -> RuntimeError {
        match self.wait_for_startup().await {
            Ok(RuntimeStartupState::Failed(error)) => RuntimeError::internal(error),
            _ => fallback,
        }
    }

    async fn request<T: Send>(
        &self,
        build: impl FnOnce(oneshot::Sender<RuntimeResult<T>>) -> Command + Send,
    ) -> RuntimeResult<T> {
        let (response, result) = oneshot::channel();
        if self.command_tx.send(build(response)).await.is_err() {
            return Err(self.transport_error(Self::command_channel_closed()).await);
        }
        match result.await {
            Ok(result) => result,
            Err(_) => Err(self.transport_error(Self::response_channel_closed()).await),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.event_tx.subscribe()
    }

    pub fn subscribe_session(&self, session_id: &SessionId) -> broadcast::Receiver<RuntimeEvent> {
        self.event_tx.subscribe_session(session_id)
    }

    pub fn startup_status(&self) -> RuntimeStartupState {
        self.startup_rx.borrow().clone()
    }

    pub async fn wait_for_startup(&self) -> RuntimeResult<RuntimeStartupState> {
        let mut startup = self.startup_rx.clone();
        loop {
            let state = startup.borrow_and_update().clone();
            if state != RuntimeStartupState::Starting {
                return Ok(state);
            }
            startup
                .changed()
                .await
                .map_err(|_| Self::response_channel_closed())?;
        }
    }

    /// Stop the runtime and wait for its host task or background thread.
    pub async fn shutdown(&self) -> RuntimeResult<()> {
        match &self.lifecycle {
            Some(lifecycle) => lifecycle.shutdown(&self.command_tx).await,
            None => lifecycle::request_shutdown(&self.command_tx).await,
        }
    }

    /// List available models from all providers
    pub async fn list_models(&self) -> RuntimeResult<HashMap<String, Vec<Model>>> {
        self.request(|response| Command::ListModels { response })
            .await
    }

    pub async fn list_provider_definitions(&self) -> RuntimeResult<Vec<ProviderDefinition>> {
        self.request(|response| Command::ListProviderDefinitions { response })
            .await
    }

    /// Get the editable settings document.
    pub async fn get_settings(&self) -> RuntimeResult<SettingsDocument> {
        self.request(|response| Command::GetSettings { response })
            .await
    }

    pub async fn list_agent_profiles(
        &self,
        session_id: SessionId,
    ) -> RuntimeResult<AgentProfilesState> {
        self.request(|response| Command::ListAgentProfiles {
            session_id,
            response,
        })
        .await
    }

    pub async fn get_agent_profile_catalog(
        &self,
        workspace_dir: Option<String>,
    ) -> RuntimeResult<AgentProfileCatalog> {
        self.request(|response| Command::GetAgentProfileCatalog {
            workspace_dir,
            response,
        })
        .await
    }

    pub async fn set_session_profile(
        &self,
        session_id: SessionId,
        profile_id: String,
    ) -> RuntimeResult<()> {
        self.request(|response| Command::SetSessionProfile {
            session_id,
            profile_id,
            response,
        })
        .await
    }

    pub async fn get_session_model(
        &self,
        session_id: SessionId,
    ) -> RuntimeResult<Option<kraai_types::ModelSelection>> {
        self.request(|response| Command::GetSessionModel {
            session_id,
            response,
        })
        .await
    }

    pub async fn set_session_model(
        &self,
        session_id: SessionId,
        selection: kraai_types::ModelSelection,
    ) -> RuntimeResult<()> {
        self.request(|response| Command::SetSessionModel {
            session_id,
            selection,
            response,
        })
        .await
    }

    pub async fn set_session_mcp_servers(
        &self,
        session_id: SessionId,
        config: kraai_mcp::McpConfig,
    ) -> RuntimeResult<()> {
        self.request(|response| Command::SetSessionMcpServers {
            session_id,
            config,
            response,
        })
        .await
    }

    /// Save the editable settings document and reload providers.
    pub async fn save_settings(&self, settings: SettingsDocument) -> RuntimeResult<()> {
        self.request(|response| Command::SaveSettings { settings, response })
            .await
    }

    pub async fn create_session(&self) -> RuntimeResult<SessionId> {
        self.create_session_with(CreateSessionRequest::default())
            .await
    }

    pub async fn create_session_with(
        &self,
        request: CreateSessionRequest,
    ) -> RuntimeResult<SessionId> {
        self.request(|response| Command::CreateSession { request, response })
            .await
    }

    /// Send a message to the agent
    pub async fn send_message(
        &self,
        session_id: SessionId,
        message: String,
        model_id: String,
        provider_id: String,
        options: ModelOptionValues,
    ) -> RuntimeResult<SubmitMessageOutcome> {
        self.send_content(session_id, message.into(), model_id, provider_id, options)
            .await
    }

    pub async fn import_image(
        &self,
        bytes: Vec<u8>,
    ) -> RuntimeResult<kraai_execution_types::ImageAttachment> {
        self.request(|response| Command::ImportImage { bytes, response })
            .await
    }

    pub async fn read_image(
        &self,
        image: kraai_execution_types::ImageAttachment,
    ) -> RuntimeResult<Vec<u8>> {
        self.request(|response| Command::ReadImage { image, response })
            .await
    }

    pub async fn send_content(
        &self,
        session_id: SessionId,
        message: kraai_types::MessageContent,
        model_id: String,
        provider_id: String,
        options: ModelOptionValues,
    ) -> RuntimeResult<SubmitMessageOutcome> {
        let model_id = ModelId::try_new(model_id).map_err(|error| {
            RuntimeError::invalid_argument(format!("invalid model_id: {error}"))
        })?;
        let provider_id = ProviderId::try_new(provider_id).map_err(|error| {
            RuntimeError::invalid_argument(format!("invalid provider_id: {error}"))
        })?;
        self.request(|response| Command::SendMessage {
            session_id,
            message,
            model_id,
            provider_id,
            options,
            response,
        })
        .await
    }

    /// Reads up to `limit` messages of the session's history ending at `from`, or at the latest
    /// message when `from` is `None`.
    pub async fn get_history_page(
        &self,
        session_id: SessionId,
        from: Option<MessageId>,
        limit: usize,
    ) -> RuntimeResult<crate::HistoryPage> {
        self.request(|response| Command::GetHistoryPage {
            session_id,
            from,
            limit,
            response,
        })
        .await
    }

    pub async fn get_session_snapshot(
        &self,
        session_id: SessionId,
    ) -> RuntimeResult<SessionSnapshot> {
        self.request(|response| Command::GetSessionSnapshot {
            session_id,
            response,
        })
        .await
    }

    pub async fn get_session_context_usage(
        &self,
        session_id: SessionId,
    ) -> RuntimeResult<Option<SessionContextUsage>> {
        self.request(|response| Command::GetSessionContextUsage {
            session_id,
            response,
        })
        .await
    }

    /// Load a session by ID
    pub async fn load_session(&self, session_id: SessionId) -> RuntimeResult<bool> {
        self.request(|response| Command::LoadSession {
            session_id,
            response,
        })
        .await
    }

    /// List all sessions
    pub async fn list_sessions(&self) -> RuntimeResult<Vec<Session>> {
        self.request(|response| Command::ListSessions { response })
            .await
    }

    pub async fn list_user_input_history(&self, limit: usize) -> RuntimeResult<Vec<String>> {
        self.request(|response| Command::ListUserInputHistory { limit, response })
            .await
    }

    /// Delete a session by ID
    pub async fn delete_session(&self, session_id: SessionId) -> RuntimeResult<()> {
        self.request(|response| Command::DeleteSession {
            session_id,
            response,
        })
        .await
    }

    pub async fn get_workspace_state(
        &self,
        session_id: SessionId,
    ) -> RuntimeResult<Option<WorkspaceState>> {
        self.request(|response| Command::GetWorkspaceState {
            session_id,
            response,
        })
        .await
    }

    pub async fn set_workspace_dir(
        &self,
        session_id: SessionId,
        workspace_dir: String,
    ) -> RuntimeResult<()> {
        self.request(|response| Command::SetWorkspaceDir {
            session_id,
            workspace_dir,
            response,
        })
        .await
    }

    /// Get the current tip message ID for a session.
    pub async fn get_tip(&self, session_id: SessionId) -> RuntimeResult<Option<String>> {
        self.request(|response| Command::GetTip {
            session_id,
            response,
        })
        .await
    }

    pub async fn undo_last_user_message(
        &self,
        session_id: SessionId,
    ) -> RuntimeResult<Option<kraai_types::MessageContent>> {
        self.request(|response| Command::UndoLastUserMessage {
            session_id,
            response,
        })
        .await
    }

    pub async fn get_pending_script(
        &self,
        session_id: SessionId,
    ) -> RuntimeResult<Option<crate::PendingScriptInfo>> {
        self.request(|response| Command::GetPendingScript {
            session_id,
            response,
        })
        .await
    }

    pub async fn approve_script(
        &self,
        session_id: SessionId,
        execution_id: String,
    ) -> RuntimeResult<()> {
        let execution_id = ScriptExecutionId::try_new(execution_id).map_err(|error| {
            RuntimeError::invalid_argument(format!("invalid execution_id: {error}"))
        })?;
        self.request(|response| Command::ApproveScript {
            session_id,
            execution_id,
            response,
        })
        .await
    }

    pub async fn deny_script(
        &self,
        session_id: SessionId,
        execution_id: String,
    ) -> RuntimeResult<()> {
        let execution_id = ScriptExecutionId::try_new(execution_id).map_err(|error| {
            RuntimeError::invalid_argument(format!("invalid execution_id: {error}"))
        })?;
        self.request(|response| Command::DenyScript {
            session_id,
            execution_id,
            response,
        })
        .await
    }

    /// Cancel the active stream for a session.
    pub async fn cancel_stream(&self, session_id: SessionId) -> RuntimeResult<bool> {
        self.request(|response| Command::CancelStream {
            session_id,
            response,
        })
        .await
    }

    pub async fn cancel_turn(&self, session_id: SessionId) -> RuntimeResult<bool> {
        self.request(|response| Command::CancelTurn {
            session_id,
            response,
        })
        .await
    }

    /// The sign-in state of every provider type that signs in with an account.
    pub async fn get_provider_auth_statuses(
        &self,
    ) -> RuntimeResult<Vec<crate::ProviderAuthStatus>> {
        self.request(|response| Command::GetProviderAuthStatuses { response })
            .await
    }

    pub async fn get_mcp_auth_statuses(&self) -> RuntimeResult<Vec<crate::McpAuthStatus>> {
        self.request(|response| Command::GetMcpAuthStatuses { response })
            .await
    }

    pub async fn start_mcp_login(&self, server: String) -> RuntimeResult<crate::McpAuthStatus> {
        self.request(|response| Command::StartMcpLogin { server, response })
            .await
    }

    pub async fn cancel_mcp_login(&self, server: String) -> RuntimeResult<crate::McpAuthStatus> {
        self.request(|response| Command::CancelMcpLogin { server, response })
            .await
    }

    pub async fn logout_mcp(&self, server: String) -> RuntimeResult<crate::McpAuthStatus> {
        self.request(|response| Command::LogoutMcp { server, response })
            .await
    }

    pub async fn start_provider_login(
        &self,
        provider_type: String,
        method: crate::LoginMethod,
    ) -> RuntimeResult<crate::ProviderAuthStatus> {
        self.request(|response| Command::StartProviderLogin {
            provider_type,
            method,
            response,
        })
        .await
    }

    pub async fn cancel_provider_login(
        &self,
        provider_type: String,
    ) -> RuntimeResult<crate::ProviderAuthStatus> {
        self.request(|response| Command::CancelProviderLogin {
            provider_type,
            response,
        })
        .await
    }

    pub async fn logout_provider(
        &self,
        provider_type: String,
    ) -> RuntimeResult<crate::ProviderAuthStatus> {
        self.request(|response| Command::LogoutProvider {
            provider_type,
            response,
        })
        .await
    }
}

/// Lists every request method of [`RuntimeHandle`] for bindings that expose it over a wire,
/// by calling `$callback! { json { name(arg: Type, ...) -> Output; ... } binary { ... } }`.
/// `binary` methods take or return raw bytes. Add every new request method here.
#[macro_export]
macro_rules! runtime_methods {
    ($callback:ident) => {
        $callback! {
            json {
                wait_for_startup() -> $crate::RuntimeStartupState;
                list_models() -> ::std::collections::HashMap<String, Vec<$crate::Model>>;
                list_provider_definitions() -> Vec<$crate::ProviderDefinition>;
                get_settings() -> $crate::SettingsDocument;
                list_agent_profiles(session_id: $crate::__types::SessionId) -> $crate::AgentProfilesState;
                get_agent_profile_catalog(workspace_dir: Option<String>) -> $crate::AgentProfileCatalog;
                set_session_profile(session_id: $crate::__types::SessionId, profile_id: String) -> ();
                get_session_model(session_id: $crate::__types::SessionId) -> Option<$crate::__types::ModelSelection>;
                set_session_model(session_id: $crate::__types::SessionId, selection: $crate::__types::ModelSelection) -> ();
                set_session_mcp_servers(session_id: $crate::__types::SessionId, config: $crate::McpConfig) -> ();
                save_settings(settings: $crate::SettingsDocument) -> ();
                create_session() -> $crate::__types::SessionId;
                create_session_with(request: $crate::CreateSessionRequest) -> $crate::__types::SessionId;
                send_content(session_id: $crate::__types::SessionId, message: $crate::__types::MessageContent, model_id: String, provider_id: String, options: $crate::__types::ModelOptionValues) -> $crate::SubmitMessageOutcome;
                send_message(session_id: $crate::__types::SessionId, message: String, model_id: String, provider_id: String, options: $crate::__types::ModelOptionValues) -> $crate::SubmitMessageOutcome;
                get_history_page(session_id: $crate::__types::SessionId, from: Option<$crate::MessageId>, limit: usize) -> $crate::HistoryPage;
                get_session_snapshot(session_id: $crate::__types::SessionId) -> $crate::SessionSnapshot;
                get_session_context_usage(session_id: $crate::__types::SessionId) -> Option<$crate::SessionContextUsage>;
                load_session(session_id: $crate::__types::SessionId) -> bool;
                list_sessions() -> Vec<$crate::Session>;
                list_user_input_history(limit: usize) -> Vec<String>;
                delete_session(session_id: $crate::__types::SessionId) -> ();
                get_workspace_state(session_id: $crate::__types::SessionId) -> Option<$crate::WorkspaceState>;
                set_workspace_dir(session_id: $crate::__types::SessionId, workspace_dir: String) -> ();
                get_tip(session_id: $crate::__types::SessionId) -> Option<String>;
                undo_last_user_message(session_id: $crate::__types::SessionId) -> Option<$crate::__types::MessageContent>;
                get_pending_script(session_id: $crate::__types::SessionId) -> Option<$crate::PendingScriptInfo>;
                approve_script(session_id: $crate::__types::SessionId, execution_id: String) -> ();
                deny_script(session_id: $crate::__types::SessionId, execution_id: String) -> ();
                cancel_stream(session_id: $crate::__types::SessionId) -> bool;
                cancel_turn(session_id: $crate::__types::SessionId) -> bool;
                get_provider_auth_statuses() -> Vec<$crate::ProviderAuthStatus>;
                start_provider_login(provider_type: String, method: $crate::LoginMethod) -> $crate::ProviderAuthStatus;
                cancel_provider_login(provider_type: String) -> $crate::ProviderAuthStatus;
                logout_provider(provider_type: String) -> $crate::ProviderAuthStatus;
                get_mcp_auth_statuses() -> Vec<$crate::McpAuthStatus>;
                start_mcp_login(server: String) -> $crate::McpAuthStatus;
                cancel_mcp_login(server: String) -> $crate::McpAuthStatus;
                logout_mcp(server: String) -> $crate::McpAuthStatus;
            }
            binary {
                import_image(bytes: Vec<u8>) -> $crate::__types::ImageAttachment;
                read_image(image: $crate::__types::ImageAttachment) -> Vec<u8>;
            }
        }
    };
}

#[cfg(test)]
mod method_list {
    macro_rules! assert_handle_methods {
        (json { $($name:ident($($arg:ident: $ty:ty),*) -> $out:ty;)* } binary { $($bname:ident($($barg:ident: $bty:ty),*) -> $bout:ty;)* }) => {
            #[test]
            fn listed_methods_exist_on_the_handle() {
                $(let _ = crate::RuntimeHandle::$name;)*
                $(let _ = crate::RuntimeHandle::$bname;)*
            }
        };
    }

    crate::runtime_methods!(assert_handle_methods);
}
