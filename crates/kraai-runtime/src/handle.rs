use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use kraai_provider_core::ProviderDefinition;
use kraai_types::{MessageId, ModelId, ProviderId, ScriptExecutionId};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::RuntimeError;
use crate::{
    AgentProfileCatalog, AgentProfilesState, ContinueSessionOutcome, CreateSessionRequest, Model,
    OpenAiCodexAuthStatus, RuntimeEvent, RuntimeResult, RuntimeStartupState, Session,
    SessionContextUsage, SessionSnapshot, SettingsDocument, SubmitMessageOutcome, WorkspaceState,
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

    async fn request<T: Send>(
        &self,
        build: impl FnOnce(oneshot::Sender<RuntimeResult<T>>) -> Command + Send,
    ) -> RuntimeResult<T> {
        let (response, result) = oneshot::channel();
        self.command_tx
            .send(build(response))
            .await
            .map_err(|_| Self::command_channel_closed())?;
        result.await.map_err(|_| Self::response_channel_closed())?
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.event_tx.subscribe()
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
        session_id: String,
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
        session_id: String,
        profile_id: String,
    ) -> RuntimeResult<()> {
        self.request(|response| Command::SetSessionProfile {
            session_id,
            profile_id,
            response,
        })
        .await
    }

    /// Save the editable settings document and reload providers.
    pub async fn save_settings(&self, settings: SettingsDocument) -> RuntimeResult<()> {
        self.request(|response| Command::SaveSettings { settings, response })
            .await
    }

    pub async fn create_session(&self) -> RuntimeResult<String> {
        self.create_session_with(CreateSessionRequest::default())
            .await
    }

    pub async fn create_session_with(
        &self,
        request: CreateSessionRequest,
    ) -> RuntimeResult<String> {
        self.request(|response| Command::CreateSession { request, response })
            .await
    }

    /// Send a message to the agent
    pub async fn send_message(
        &self,
        session_id: String,
        message: String,
        model_id: String,
        provider_id: String,
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
            response,
        })
        .await
    }

    /// Get the chat history as a tree
    pub async fn get_chat_history(
        &self,
        session_id: String,
    ) -> RuntimeResult<BTreeMap<MessageId, kraai_types::Message>> {
        self.request(|response| Command::GetChatHistory {
            session_id,
            response,
        })
        .await
    }

    pub async fn get_session_snapshot(&self, session_id: String) -> RuntimeResult<SessionSnapshot> {
        self.request(|response| Command::GetSessionSnapshot {
            session_id,
            response,
        })
        .await
    }

    pub async fn get_session_context_usage(
        &self,
        session_id: String,
    ) -> RuntimeResult<Option<SessionContextUsage>> {
        self.request(|response| Command::GetSessionContextUsage {
            session_id,
            response,
        })
        .await
    }

    /// Load a session by ID
    pub async fn load_session(&self, session_id: String) -> RuntimeResult<bool> {
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
    pub async fn delete_session(&self, session_id: String) -> RuntimeResult<()> {
        self.request(|response| Command::DeleteSession {
            session_id,
            response,
        })
        .await
    }

    pub async fn get_workspace_state(
        &self,
        session_id: String,
    ) -> RuntimeResult<Option<WorkspaceState>> {
        self.request(|response| Command::GetWorkspaceState {
            session_id,
            response,
        })
        .await
    }

    pub async fn set_workspace_dir(
        &self,
        session_id: String,
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
    pub async fn get_tip(&self, session_id: String) -> RuntimeResult<Option<String>> {
        self.request(|response| Command::GetTip {
            session_id,
            response,
        })
        .await
    }

    pub async fn undo_last_user_message(
        &self,
        session_id: String,
    ) -> RuntimeResult<Option<String>> {
        self.request(|response| Command::UndoLastUserMessage {
            session_id,
            response,
        })
        .await
    }

    pub async fn get_pending_script(
        &self,
        session_id: String,
    ) -> RuntimeResult<Option<crate::PendingScriptInfo>> {
        self.request(|response| Command::GetPendingScript {
            session_id,
            response,
        })
        .await
    }

    pub async fn approve_script(
        &self,
        session_id: String,
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

    pub async fn deny_script(&self, session_id: String, execution_id: String) -> RuntimeResult<()> {
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
    pub async fn cancel_stream(&self, session_id: String) -> RuntimeResult<bool> {
        self.request(|response| Command::CancelStream {
            session_id,
            response,
        })
        .await
    }

    pub async fn continue_session(
        &self,
        session_id: String,
    ) -> RuntimeResult<ContinueSessionOutcome> {
        self.request(|response| Command::ContinueSession {
            session_id,
            response,
        })
        .await
    }

    pub async fn get_openai_codex_auth_status(&self) -> RuntimeResult<OpenAiCodexAuthStatus> {
        self.request(|response| Command::GetOpenAiCodexAuthStatus { response })
            .await
    }

    pub async fn start_openai_codex_browser_login(&self) -> RuntimeResult<()> {
        self.request(|response| Command::StartOpenAiCodexBrowserLogin { response })
            .await
    }

    pub async fn start_openai_codex_device_code_login(&self) -> RuntimeResult<()> {
        self.request(|response| Command::StartOpenAiCodexDeviceCodeLogin { response })
            .await
    }

    pub async fn cancel_openai_codex_login(&self) -> RuntimeResult<()> {
        self.request(|response| Command::CancelOpenAiCodexLogin { response })
            .await
    }

    pub async fn logout_openai_codex_auth(&self) -> RuntimeResult<()> {
        self.request(|response| Command::LogoutOpenAiCodexAuth { response })
            .await
    }
}
