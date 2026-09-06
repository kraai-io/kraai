use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kraai_provider_core::ProviderDefinition;
use kraai_types::{MessageId, ModelId, ProviderId, ScriptExecutionId};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::RuntimeError;
use crate::{
    AgentProfileCatalog, AgentProfilesState, ContinueSessionOutcome, CreateSessionRequest, Event,
    Model, OpenAiCodexAuthStatus, RuntimeEvent, RuntimeResult, RuntimeStartupState, Session,
    SessionContextUsage, SessionSnapshot, SettingsDocument, SubmitMessageOutcome, WorkspaceState,
};

#[derive(Clone)]
pub(crate) struct RuntimeEventSender {
    tx: broadcast::Sender<RuntimeEvent>,
    sequence: Arc<AtomicU64>,
}

impl RuntimeEventSender {
    pub(crate) fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            tx,
            sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn send(&self, event: Event) {
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.tx.send(RuntimeEvent { sequence, event });
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.tx.subscribe()
    }

    pub(crate) fn latest_sequence(&self) -> u64 {
        self.sequence.load(Ordering::SeqCst)
    }
}

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

pub(crate) struct RuntimeLifecycle {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    shutdown_started: AtomicBool,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl RuntimeLifecycle {
    pub(crate) fn new(shutdown_tx: tokio::sync::watch::Sender<bool>) -> Self {
        Self {
            shutdown_tx,
            shutdown_started: AtomicBool::new(false),
            thread: std::sync::Mutex::new(None),
            task: std::sync::Mutex::new(None),
        }
    }

    pub(crate) fn set_thread(&self, thread: std::thread::JoinHandle<()>) {
        if let Ok(mut slot) = self.thread.lock() {
            *slot = Some(thread);
        }
    }

    pub(crate) fn set_task(&self, task: tokio::task::JoinHandle<()>) {
        if let Ok(mut slot) = self.task.lock() {
            *slot = Some(task);
        }
    }
}

impl Drop for RuntimeLifecycle {
    fn drop(&mut self) {
        if !self.shutdown_started.swap(true, Ordering::SeqCst) {
            self.shutdown_tx.send_replace(true);
        }
    }
}

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
        let should_signal = self
            .lifecycle
            .as_ref()
            .is_none_or(|lifecycle| !lifecycle.shutdown_started.swap(true, Ordering::SeqCst));
        if should_signal {
            let (tx, rx) = oneshot::channel();
            if self
                .command_tx
                .send(Command::Shutdown { response: Some(tx) })
                .await
                .is_ok()
            {
                rx.await.map_err(|_| Self::response_channel_closed())??;
            }
        }

        let thread = self.lifecycle.as_ref().and_then(|lifecycle| {
            lifecycle
                .thread
                .lock()
                .ok()
                .and_then(|mut thread| thread.take())
        });
        if let Some(thread) = thread {
            tokio::task::spawn_blocking(move || thread.join())
                .await
                .map_err(RuntimeError::internal)?
                .map_err(|_panic| RuntimeError::internal("runtime background thread panicked"))?;
        }
        let task = self
            .lifecycle
            .as_ref()
            .and_then(|lifecycle| lifecycle.task.lock().ok().and_then(|mut task| task.take()));
        if let Some(task) = task {
            task.await
                .map_err(|error| RuntimeError::internal(format!("runtime task failed: {error}")))?;
        }
        Ok(())
    }

    /// List available models from all providers
    pub async fn list_models(&self) -> RuntimeResult<HashMap<String, Vec<Model>>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::ListModels { response: tx })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn list_provider_definitions(&self) -> RuntimeResult<Vec<ProviderDefinition>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::ListProviderDefinitions { response: tx })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    /// Get the editable settings document.
    pub async fn get_settings(&self) -> RuntimeResult<SettingsDocument> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetSettings { response: tx })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn list_agent_profiles(
        &self,
        session_id: String,
    ) -> RuntimeResult<AgentProfilesState> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::ListAgentProfiles {
                session_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn get_agent_profile_catalog(
        &self,
        workspace_dir: Option<String>,
    ) -> RuntimeResult<AgentProfileCatalog> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetAgentProfileCatalog {
                workspace_dir,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn set_session_profile(
        &self,
        session_id: String,
        profile_id: String,
    ) -> RuntimeResult<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SetSessionProfile {
                session_id,
                profile_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    /// Save the editable settings document and reload providers.
    pub async fn save_settings(&self, settings: SettingsDocument) -> RuntimeResult<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SaveSettings {
                settings,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn create_session(&self) -> RuntimeResult<String> {
        self.create_session_with(CreateSessionRequest::default())
            .await
    }

    pub async fn create_session_with(
        &self,
        request: CreateSessionRequest,
    ) -> RuntimeResult<String> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::CreateSession {
                request,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
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
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SendMessage {
                session_id,
                message,
                model_id,
                provider_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    /// Get the chat history as a tree
    pub async fn get_chat_history(
        &self,
        session_id: String,
    ) -> RuntimeResult<BTreeMap<MessageId, kraai_types::Message>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetChatHistory {
                session_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn get_session_snapshot(&self, session_id: String) -> RuntimeResult<SessionSnapshot> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetSessionSnapshot {
                session_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn get_session_context_usage(
        &self,
        session_id: String,
    ) -> RuntimeResult<Option<SessionContextUsage>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetSessionContextUsage {
                session_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    /// Load a session by ID
    pub async fn load_session(&self, session_id: String) -> RuntimeResult<bool> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::LoadSession {
                session_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    /// List all sessions
    pub async fn list_sessions(&self) -> RuntimeResult<Vec<Session>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::ListSessions { response: tx })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn list_user_input_history(&self, limit: usize) -> RuntimeResult<Vec<String>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::ListUserInputHistory {
                limit,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    /// Delete a session by ID
    pub async fn delete_session(&self, session_id: String) -> RuntimeResult<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::DeleteSession {
                session_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn get_workspace_state(
        &self,
        session_id: String,
    ) -> RuntimeResult<Option<WorkspaceState>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetWorkspaceState {
                session_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn set_workspace_dir(
        &self,
        session_id: String,
        workspace_dir: String,
    ) -> RuntimeResult<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SetWorkspaceDir {
                session_id,
                workspace_dir,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    /// Get the current tip message ID for a session.
    pub async fn get_tip(&self, session_id: String) -> RuntimeResult<Option<String>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetTip {
                session_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn undo_last_user_message(
        &self,
        session_id: String,
    ) -> RuntimeResult<Option<String>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::UndoLastUserMessage {
                session_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn get_pending_script(
        &self,
        session_id: String,
    ) -> RuntimeResult<Option<crate::PendingScriptInfo>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetPendingScript {
                session_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn approve_script(
        &self,
        session_id: String,
        execution_id: String,
    ) -> RuntimeResult<()> {
        let execution_id = ScriptExecutionId::try_new(execution_id).map_err(|error| {
            RuntimeError::invalid_argument(format!("invalid execution_id: {error}"))
        })?;
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::ApproveScript {
                session_id,
                execution_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn deny_script(&self, session_id: String, execution_id: String) -> RuntimeResult<()> {
        let execution_id = ScriptExecutionId::try_new(execution_id).map_err(|error| {
            RuntimeError::invalid_argument(format!("invalid execution_id: {error}"))
        })?;
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::DenyScript {
                session_id,
                execution_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    /// Cancel the active stream for a session.
    pub async fn cancel_stream(&self, session_id: String) -> RuntimeResult<bool> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::CancelStream {
                session_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn continue_session(
        &self,
        session_id: String,
    ) -> RuntimeResult<ContinueSessionOutcome> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::ContinueSession {
                session_id,
                response: tx,
            })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn get_openai_codex_auth_status(&self) -> RuntimeResult<OpenAiCodexAuthStatus> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetOpenAiCodexAuthStatus { response: tx })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn start_openai_codex_browser_login(&self) -> RuntimeResult<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::StartOpenAiCodexBrowserLogin { response: tx })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn start_openai_codex_device_code_login(&self) -> RuntimeResult<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::StartOpenAiCodexDeviceCodeLogin { response: tx })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn cancel_openai_codex_login(&self) -> RuntimeResult<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::CancelOpenAiCodexLogin { response: tx })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }

    pub async fn logout_openai_codex_auth(&self) -> RuntimeResult<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::LogoutOpenAiCodexAuth { response: tx })
            .await
            .map_err(|_| Self::command_channel_closed())?;
        rx.await.map_err(|_| Self::response_channel_closed())?
    }
}
