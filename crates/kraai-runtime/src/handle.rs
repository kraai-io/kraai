use std::collections::{BTreeMap, HashMap};

use color_eyre::eyre::Result;
use kraai_provider_core::ProviderDefinition;
use kraai_types::{MessageId, ModelId, ProviderId};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::{
    AgentProfilesState, Event, Model, OpenAiCodexAuthStatus, PendingToolInfo, Session,
    SessionContextUsage, SettingsDocument, WorkspaceState,
};

/// Internal commands sent to the runtime
pub(crate) enum Command {
    ListModels {
        response: oneshot::Sender<HashMap<String, Vec<Model>>>,
    },
    ListProviderDefinitions {
        response: oneshot::Sender<Vec<ProviderDefinition>>,
    },
    GetSettings {
        response: oneshot::Sender<SettingsDocument>,
    },
    ListAgentProfiles {
        session_id: String,
        response: oneshot::Sender<AgentProfilesState>,
    },
    SetSessionProfile {
        session_id: String,
        profile_id: String,
        response: oneshot::Sender<()>,
    },
    SaveSettings {
        settings: SettingsDocument,
        response: oneshot::Sender<()>,
    },
    CreateSession {
        response: oneshot::Sender<String>,
    },
    SendMessage {
        session_id: String,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
        auto_approve: bool,
    },
    StartQueuedMessages {
        session_id: String,
    },
    LoadConfig,
    LoadSession {
        session_id: String,
        response: oneshot::Sender<bool>,
    },
    ListSessions {
        response: oneshot::Sender<Vec<Session>>,
    },
    DeleteSession {
        session_id: String,
    },
    GetWorkspaceState {
        session_id: String,
        response: oneshot::Sender<Option<WorkspaceState>>,
    },
    SetWorkspaceDir {
        session_id: String,
        workspace_dir: String,
        response: oneshot::Sender<()>,
    },
    GetTip {
        session_id: String,
        response: oneshot::Sender<Option<String>>,
    },
    UndoLastUserMessage {
        session_id: String,
        response: oneshot::Sender<Option<String>>,
    },
    GetChatHistory {
        session_id: String,
        response: oneshot::Sender<BTreeMap<MessageId, kraai_types::Message>>,
    },
    GetSessionContextUsage {
        session_id: String,
        response: oneshot::Sender<Option<SessionContextUsage>>,
    },
    GetPendingTools {
        session_id: String,
        response: oneshot::Sender<Vec<PendingToolInfo>>,
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
        response: oneshot::Sender<bool>,
    },
    ContinueSession {
        session_id: String,
    },
    ExecuteApprovedTools {
        session_id: String,
    },
    GetOpenAiCodexAuthStatus {
        response: oneshot::Sender<OpenAiCodexAuthStatus>,
    },
    StartOpenAiCodexBrowserLogin {
        response: oneshot::Sender<()>,
    },
    StartOpenAiCodexDeviceCodeLogin {
        response: oneshot::Sender<()>,
    },
    CancelOpenAiCodexLogin {
        response: oneshot::Sender<()>,
    },
    LogoutOpenAiCodexAuth {
        response: oneshot::Sender<()>,
    },
}

/// Handle to the runtime for sending commands
///
/// This is cheaply cloneable and can be passed around to different parts
/// of the application.
#[derive(Clone)]
pub struct RuntimeHandle {
    pub(crate) command_tx: mpsc::Sender<Command>,
    pub(crate) event_tx: broadcast::Sender<Event>,
}

impl RuntimeHandle {
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.event_tx.subscribe()
    }

    /// List available models from all providers
    pub async fn list_models(&self) -> Result<HashMap<String, Vec<Model>>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::ListModels { response: tx })
            .await?;
        Ok(rx.await?)
    }

    pub async fn list_provider_definitions(&self) -> Result<Vec<ProviderDefinition>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::ListProviderDefinitions { response: tx })
            .await?;
        Ok(rx.await?)
    }

    /// Get the editable settings document.
    pub async fn get_settings(&self) -> Result<SettingsDocument> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetSettings { response: tx })
            .await?;
        Ok(rx.await?)
    }

    pub async fn list_agent_profiles(&self, session_id: String) -> Result<AgentProfilesState> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::ListAgentProfiles {
                session_id,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    pub async fn set_session_profile(&self, session_id: String, profile_id: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SetSessionProfile {
                session_id,
                profile_id,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    /// Save the editable settings document and reload providers.
    pub async fn save_settings(&self, settings: SettingsDocument) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SaveSettings {
                settings,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    pub async fn create_session(&self) -> Result<String> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::CreateSession { response: tx })
            .await?;
        Ok(rx.await?)
    }

    /// Send a message to the agent
    pub async fn send_message(
        &self,
        session_id: String,
        message: String,
        model_id: String,
        provider_id: String,
    ) -> Result<()> {
        self.send_message_with_options(session_id, message, model_id, provider_id, false)
            .await
    }

    pub async fn send_message_with_options(
        &self,
        session_id: String,
        message: String,
        model_id: String,
        provider_id: String,
        auto_approve: bool,
    ) -> Result<()> {
        self.command_tx
            .send(Command::SendMessage {
                session_id,
                message,
                model_id: ModelId::new(model_id),
                provider_id: ProviderId::new(provider_id),
                auto_approve,
            })
            .await?;
        Ok(())
    }

    /// Get the chat history as a tree
    pub async fn get_chat_history(
        &self,
        session_id: String,
    ) -> Result<BTreeMap<MessageId, kraai_types::Message>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetChatHistory {
                session_id,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    pub async fn get_session_context_usage(
        &self,
        session_id: String,
    ) -> Result<Option<SessionContextUsage>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetSessionContextUsage {
                session_id,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    /// Load a session by ID
    pub async fn load_session(&self, session_id: String) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::LoadSession {
                session_id,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    /// List all sessions
    pub async fn list_sessions(&self) -> Result<Vec<Session>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::ListSessions { response: tx })
            .await?;
        Ok(rx.await?)
    }

    /// Delete a session by ID
    pub async fn delete_session(&self, session_id: String) -> Result<()> {
        self.command_tx
            .send(Command::DeleteSession { session_id })
            .await?;
        Ok(())
    }

    pub async fn get_workspace_state(&self, session_id: String) -> Result<Option<WorkspaceState>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetWorkspaceState {
                session_id,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    pub async fn set_workspace_dir(&self, session_id: String, workspace_dir: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SetWorkspaceDir {
                session_id,
                workspace_dir,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    /// Get the current tip message ID for a session.
    pub async fn get_tip(&self, session_id: String) -> Result<Option<String>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetTip {
                session_id,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    pub async fn undo_last_user_message(&self, session_id: String) -> Result<Option<String>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::UndoLastUserMessage {
                session_id,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    pub async fn get_pending_tools(&self, session_id: String) -> Result<Vec<PendingToolInfo>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetPendingTools {
                session_id,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    /// Approve a tool call
    pub async fn approve_tool(&self, session_id: String, call_id: String) -> Result<()> {
        self.command_tx
            .send(Command::ApproveTool {
                session_id,
                call_id,
            })
            .await?;
        Ok(())
    }

    /// Deny a tool call
    pub async fn deny_tool(&self, session_id: String, call_id: String) -> Result<()> {
        self.command_tx
            .send(Command::DenyTool {
                session_id,
                call_id,
            })
            .await?;
        Ok(())
    }

    /// Cancel the active stream for a session.
    pub async fn cancel_stream(&self, session_id: String) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::CancelStream {
                session_id,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    pub async fn continue_session(&self, session_id: String) -> Result<()> {
        self.command_tx
            .send(Command::ContinueSession { session_id })
            .await?;
        Ok(())
    }

    /// Execute all approved tools
    pub async fn execute_approved_tools(&self, session_id: String) -> Result<()> {
        self.command_tx
            .send(Command::ExecuteApprovedTools { session_id })
            .await?;
        Ok(())
    }

    pub async fn get_openai_codex_auth_status(&self) -> Result<OpenAiCodexAuthStatus> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetOpenAiCodexAuthStatus { response: tx })
            .await?;
        Ok(rx.await?)
    }

    pub async fn start_openai_codex_browser_login(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::StartOpenAiCodexBrowserLogin { response: tx })
            .await?;
        Ok(rx.await?)
    }

    pub async fn start_openai_codex_device_code_login(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::StartOpenAiCodexDeviceCodeLogin { response: tx })
            .await?;
        Ok(rx.await?)
    }

    pub async fn cancel_openai_codex_login(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::CancelOpenAiCodexLogin { response: tx })
            .await?;
        Ok(rx.await?)
    }

    pub async fn logout_openai_codex_auth(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::LogoutOpenAiCodexAuth { response: tx })
            .await?;
        Ok(rx.await?)
    }
}
