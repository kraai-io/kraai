#![forbid(unsafe_code)]
#![deny(clippy::all)]

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use agent::{AgentManager, PendingStreamRequest, ToolExecutionPayload, ToolExecutionRequest};
use color_eyre::eyre::{Context, Result, eyre};
use persistence::{SessionMeta, agent_state_root};
use provider_core::{
    DynamicConfig, ModelConfig, ProviderConfig, ProviderManager, ProviderManagerConfig,
    ProviderRegistry,
};
use tool_close_file::CloseFileTool;
use tool_core::{ToolContext, ToolManager, ToolOutput};
use tool_edit_file::EditFileTool;
use tool_list_files::ListFilesTool;
use tool_open_file::OpenFileTool;
use tool_read_file::ReadFileTool;
use tool_search_files::SearchFilesTool;
use types::{MessageId, ModelId, ProviderId};

use futures::StreamExt;
use notify::{RecursiveMode, Watcher};
use provider_openai_chat_completions::{OpenAiChatCompletionsFactory, OpenAiFactory};
use provider_openai_codex::{
    OpenAiCodexAuthController, OpenAiCodexAuthStatus as ProviderOpenAiCodexAuthStatus,
    OpenAiCodexFactory, OpenAiCodexLoginState as ProviderOpenAiCodexLoginState,
};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::AbortHandle;

pub use provider_core::{
    DynamicValue as SettingsValue, FieldDefinition, FieldValueKind, ProviderDefinition,
};
pub use types::{AgentProfileSource, AgentProfileSummary, AgentProfileWarning, AgentProfilesState};

// ============================================================================
// Public Types - exposed to all clients
// ============================================================================

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

impl From<SessionMeta> for Session {
    fn from(meta: SessionMeta) -> Self {
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

/// Editable provider settings shared across clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSettings {
    pub id: String,
    pub type_id: String,
    pub values: Vec<FieldValueEntry>,
}

/// Editable model settings shared across clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSettings {
    pub id: String,
    pub provider_id: String,
    pub values: Vec<FieldValueEntry>,
}

/// Full editable settings document persisted to providers.toml.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsDocument {
    pub providers: Vec<ProviderSettings>,
    pub models: Vec<ModelSettings>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldValueEntry {
    pub key: String,
    pub value: SettingsValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SettingsValidationError {
    field: String,
    message: String,
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

// ============================================================================
// Event Callback Trait - clients implement this
// ============================================================================

/// Trait for receiving events from the runtime
///
/// Clients (TUI, CLI, Electron, etc.) implement this trait to receive
/// events from the runtime.
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

// ============================================================================
// Internal Commands
// ============================================================================

/// Internal commands sent to the runtime
enum Command {
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
    GetChatHistory {
        session_id: String,
        response: oneshot::Sender<BTreeMap<MessageId, types::Message>>,
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

// ============================================================================
// Runtime Handle - cheaply cloneable handle to send commands
// ============================================================================

/// Handle to the runtime for sending commands
///
/// This is cheaply cloneable and can be passed around to different parts
/// of the application.
#[derive(Clone)]
pub struct RuntimeHandle {
    command_tx: mpsc::Sender<Command>,
}

impl RuntimeHandle {
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
        self.command_tx
            .send(Command::SendMessage {
                session_id,
                message,
                model_id: ModelId::new(model_id),
                provider_id: ProviderId::new(provider_id),
            })
            .await?;
        Ok(())
    }

    /// Get the chat history as a tree
    pub async fn get_chat_history(
        &self,
        session_id: String,
    ) -> Result<BTreeMap<MessageId, types::Message>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetChatHistory {
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

// ============================================================================
// Runtime Builder
// ============================================================================

/// Builder for creating a runtime
pub struct RuntimeBuilder {
    callback: Arc<dyn EventCallback>,
}

impl RuntimeBuilder {
    /// Create a new runtime builder with the given event callback
    pub fn new(callback: impl EventCallback + 'static) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }

    /// Build and start the runtime
    ///
    /// This spawns the runtime in a background thread and returns a handle
    /// to send commands.
    pub fn build(self) -> RuntimeHandle {
        let (command_tx, command_rx) = mpsc::channel(100);
        let handle = RuntimeHandle { command_tx };
        let command_tx_for_runtime = handle.command_tx.clone();

        let callback = self.callback.clone();

        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(error) => {
                    callback.on_event(Event::Error(format!(
                        "Failed to create tokio runtime: {error}"
                    )));
                    return;
                }
            };

            if let Err(error) = rt.block_on(Self::run_background(
                callback.clone(),
                command_tx_for_runtime,
                command_rx,
            )) {
                callback.on_event(Event::Error(error.to_string()));
            }
        });

        handle
    }

    async fn run_background(
        callback: Arc<dyn EventCallback>,
        command_tx: mpsc::Sender<Command>,
        command_rx: mpsc::Receiver<Command>,
    ) -> Result<()> {
        Self::init_tracing()?;

        let (message_store, session_store) = persistence::init()
            .await
            .wrap_err("Failed to initialize persistence layer")?;

        let providers = ProviderManager::new();
        let default_workspace_dir = std::env::current_dir()
            .and_then(|path| path.canonicalize())
            .or_else(|_| std::env::current_dir())
            .wrap_err("Failed to determine current workspace directory")?;
        let tools = build_default_tool_manager();
        let openai_codex_auth = Arc::new(
            OpenAiCodexAuthController::new().wrap_err("Failed to initialize OpenAI auth")?,
        );
        let registry = build_provider_registry(openai_codex_auth.clone())?;

        let agent_manager = Arc::new(Mutex::new(AgentManager::new(
            providers,
            tools,
            default_workspace_dir,
            message_store,
            session_store,
        )));

        let runtime = RuntimeInner {
            event_callback: callback,
            command_tx,
            agent_manager,
            provider_registry: registry,
            active_streams: Arc::new(Mutex::new(HashMap::new())),
            openai_codex_auth,
        };

        runtime.run(command_rx).await;
        Ok(())
    }

    fn init_tracing() -> Result<()> {
        use std::sync::{Mutex, Once};
        static INIT: Once = Once::new();
        static TRACING_INIT_RESULT: Mutex<Option<Result<(), String>>> = Mutex::new(None);

        INIT.call_once(|| {
            let result = (|| -> Result<()> {
                let log_dir = agent_state_root()?.join("logs");

                std::fs::create_dir_all(&log_dir).wrap_err_with(|| {
                    format!("Failed to create log directory {}", log_dir.display())
                })?;

                let file_appender = tracing_appender::rolling::daily(&log_dir, "agent.log");

                let subscriber = tracing_subscriber::fmt()
                    .with_env_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
                    )
                    .with_writer(file_appender)
                    .with_ansi(false)
                    .finish();

                tracing::subscriber::set_global_default(subscriber)
                    .map_err(|error| eyre!("Failed to set tracing subscriber: {error}"))?;
                Ok(())
            })();

            if let Ok(mut slot) = TRACING_INIT_RESULT.lock() {
                *slot = Some(result.map_err(|error| error.to_string()));
            }
        });

        TRACING_INIT_RESULT
            .lock()
            .map_err(|_| eyre!("Tracing init mutex poisoned"))?
            .as_ref()
            .map(|result| match result {
                Ok(()) => Ok(()),
                Err(error) => Err(eyre!(error.clone())),
            })
            .unwrap_or_else(|| Ok(()))
    }
}

fn build_default_tool_manager() -> ToolManager {
    let mut tools = ToolManager::new();
    tools.register_tool(CloseFileTool);
    tools.register_tool(ReadFileTool);
    tools.register_tool(ListFilesTool);
    tools.register_tool(OpenFileTool);
    tools.register_tool(SearchFilesTool);
    tools.register_tool(EditFileTool);
    tools
}

fn build_provider_registry(
    openai_codex_auth: Arc<OpenAiCodexAuthController>,
) -> Result<ProviderRegistry> {
    let mut registry = ProviderRegistry::default();
    registry
        .register_factory::<OpenAiChatCompletionsFactory>()
        .map_err(|error| eyre!(error.to_string()))?;
    registry
        .register_factory::<OpenAiFactory>()
        .map_err(|error| eyre!(error.to_string()))?;
    let openai_codex_factory = OpenAiCodexFactory::new(openai_codex_auth);
    registry
        .register_dynamic_factory(
            OpenAiCodexFactory::TYPE_ID,
            OpenAiCodexFactory::definition(),
            move |id, config| {
                openai_codex_factory.create(id, config).map_err(|error| {
                    provider_core::ProviderError::ConfigParseError(error.to_string())
                })
            },
            OpenAiCodexFactory::validate_provider_config,
            OpenAiCodexFactory::validate_model_config,
        )
        .map_err(|error| eyre!(error.to_string()))?;
    Ok(registry)
}

async fn execute_tool_requests(executions: Vec<ToolExecutionRequest>) -> Vec<types::ToolResult> {
    let mut results = Vec::with_capacity(executions.len());

    for execution in executions {
        let (output, permission_denied, tool_state_deltas) = match execution.payload {
            ToolExecutionPayload::Denied => (
                serde_json::json!({ "error": "Permission denied by user" }),
                true,
                Vec::new(),
            ),
            ToolExecutionPayload::Approved { prepared, config } => {
                let ctx = ToolContext {
                    global_config: &config,
                };
                match prepared.call(&ctx).await {
                    ToolOutput::Success { data } => {
                        let deltas = prepared.successful_tool_state_deltas(&ctx);
                        (data, false, deltas)
                    }
                    ToolOutput::Error { message } => {
                        (serde_json::json!({ "error": message }), false, Vec::new())
                    }
                }
            }
        };

        results.push(types::ToolResult {
            call_id: execution.call_id,
            tool_id: execution.tool_id,
            output,
            permission_denied,
            tool_state_deltas,
        });
    }

    results
}

// ============================================================================
// Runtime Inner - the actual runtime implementation
// ============================================================================

#[derive(Clone)]
struct RuntimeInner {
    event_callback: Arc<dyn EventCallback>,
    command_tx: mpsc::Sender<Command>,
    agent_manager: Arc<Mutex<AgentManager>>,
    provider_registry: ProviderRegistry,
    active_streams: Arc<Mutex<HashMap<String, ActiveStream>>>,
    openai_codex_auth: Arc<OpenAiCodexAuthController>,
}

#[derive(Clone, Debug)]
struct ActiveStream {
    message_id: MessageId,
    abort_handle: AbortHandle,
}

#[derive(Debug)]
enum StreamDriveResult {
    Completed { session_id: String, content: String },
    FailedToStart { error: String },
    FailedDuringStream { error: String },
    Stopped,
}

impl RuntimeInner {
    fn send_event(&self, event: Event) {
        self.event_callback.on_event(event);
    }

    fn send_error(&self, error: impl Into<String>) {
        self.send_event(Event::Error(error.into()));
    }

    async fn run(self, mut command_rx: mpsc::Receiver<Command>) {
        tracing::info!("Starting event loop");

        self.spawn_config_watcher();
        self.spawn_openai_auth_forwarder();
        if let Err(e) = self.load_providers_config().await {
            self.send_error(format!("Failed to load config: {}", e));
        } else {
            tracing::info!("Loaded config");
            self.send_event(Event::ConfigLoaded);
        }

        while let Some(command) = command_rx.recv().await {
            if let Err(e) = self.handle_command(command).await {
                self.send_error(e.to_string());
            }
        }

        tracing::info!("Event loop terminated");
    }

    fn spawn_openai_auth_forwarder(&self) {
        let mut updates = self.openai_codex_auth.subscribe();
        let runtime = self.clone();
        tokio::spawn(async move {
            while let Ok(status) = updates.recv().await {
                runtime.send_event(Event::OpenAiCodexAuthUpdated {
                    status: map_openai_codex_auth_status(status),
                });
            }
        });
    }

    async fn handle_command(&self, command: Command) -> Result<()> {
        match command {
            Command::ListModels { response } => {
                let models_map = self.agent_manager.lock().await.list_models().await;
                let models: HashMap<String, Vec<Model>> = models_map
                    .into_iter()
                    .map(|(provider_id, model_list)| {
                        let models: Vec<Model> = model_list
                            .into_iter()
                            .map(|m| Model {
                                id: m.id.to_string(),
                                name: m.name,
                            })
                            .collect();
                        (provider_id.to_string(), models)
                    })
                    .collect();
                response
                    .send(models)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::ListProviderDefinitions { response } => {
                response
                    .send(self.provider_registry.list_definitions())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::GetSettings { response } => {
                let settings = read_settings_document(&settings_path()?, &self.provider_registry)?;
                response
                    .send(settings)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::ListAgentProfiles {
                session_id,
                response,
            } => {
                let profiles = self
                    .agent_manager
                    .lock()
                    .await
                    .list_agent_profiles(&session_id)
                    .await?;
                response
                    .send(profiles)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::SetSessionProfile {
                session_id,
                profile_id,
                response,
            } => {
                self.agent_manager
                    .lock()
                    .await
                    .set_session_profile(&session_id, profile_id)
                    .await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::SaveSettings { settings, response } => {
                self.save_settings_document(settings).await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::CreateSession { response } => {
                let session_id = self.agent_manager.lock().await.create_session().await?;
                response
                    .send(session_id)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::LoadConfig => {
                self.load_providers_config().await?;
                tracing::info!("Loaded config");
                self.send_event(Event::ConfigLoaded);
            }

            Command::SendMessage {
                session_id,
                message,
                model_id,
                provider_id,
            } => {
                self.handle_send_message(session_id, message, model_id, provider_id)
                    .await;
            }

            Command::LoadSession {
                session_id,
                response,
            } => {
                let loaded = self
                    .agent_manager
                    .lock()
                    .await
                    .prepare_session(&session_id)
                    .await?;
                response
                    .send(loaded)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::ListSessions { response } => {
                let agent = self.agent_manager.lock().await;
                let sessions = agent.list_sessions().await?;
                let streaming_sessions = agent.streaming_session_ids().await;
                let sessions: Vec<Session> = sessions
                    .into_iter()
                    .map(|session| Session {
                        profile_locked: agent.is_profile_locked(&session.id),
                        waiting_for_approval: agent.session_waiting_for_approval(&session.id),
                        is_streaming: streaming_sessions.contains(&session.id),
                        ..session.into()
                    })
                    .collect();
                response
                    .send(sessions)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::DeleteSession { session_id } => {
                if let Some(active_stream) = self.take_active_stream(&session_id).await {
                    active_stream.abort_handle.abort();
                }
                self.agent_manager
                    .lock()
                    .await
                    .delete_session(&session_id)
                    .await?;
            }

            Command::GetWorkspaceState {
                session_id,
                response,
            } => {
                let workspace_state = self
                    .agent_manager
                    .lock()
                    .await
                    .get_workspace_dir_state(&session_id)
                    .await?
                    .map(|(workspace_dir, applies_next_chat)| WorkspaceState {
                        workspace_dir: workspace_dir.display().to_string(),
                        applies_next_chat,
                    });
                response
                    .send(workspace_state)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::SetWorkspaceDir {
                session_id,
                workspace_dir,
                response,
            } => {
                let workspace_dir = canonicalize_workspace_dir(&workspace_dir)?;
                self.agent_manager
                    .lock()
                    .await
                    .set_workspace_dir(&session_id, workspace_dir)
                    .await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::GetTip {
                session_id,
                response,
            } => {
                let tip_id = self
                    .agent_manager
                    .lock()
                    .await
                    .get_tip(&session_id)
                    .await?
                    .map(|id| id.to_string());
                response
                    .send(tip_id)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::GetChatHistory {
                session_id,
                response,
            } => {
                let history = self
                    .agent_manager
                    .lock()
                    .await
                    .get_chat_history(&session_id)
                    .await?;
                response
                    .send(history)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::GetPendingTools {
                session_id,
                response,
            } => {
                let tools = self
                    .agent_manager
                    .lock()
                    .await
                    .list_pending_tools(&session_id)
                    .into_iter()
                    .map(|tool| PendingToolInfo {
                        call_id: tool.call_id.to_string(),
                        tool_id: tool.tool_id.to_string(),
                        args: serde_json::to_string(&tool.args).unwrap_or_default(),
                        description: tool.description,
                        risk_level: tool.risk_level.as_str().to_string(),
                        reasons: tool.reasons,
                        approved: tool.approved,
                        queue_order: tool.queue_order,
                    })
                    .collect();
                response
                    .send(tools)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::ApproveTool {
                session_id,
                call_id,
            } => {
                let call_id = types::CallId::new(call_id);
                self.agent_manager
                    .lock()
                    .await
                    .approve_tool(&session_id, call_id);
            }

            Command::DenyTool {
                session_id,
                call_id,
            } => {
                let call_id = types::CallId::new(call_id);
                self.agent_manager
                    .lock()
                    .await
                    .deny_tool(&session_id, call_id);
            }

            Command::CancelStream {
                session_id,
                response,
            } => {
                let cancelled = self.cancel_stream(session_id).await?;
                response
                    .send(cancelled)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::ExecuteApprovedTools { session_id } => {
                self.handle_execute_tools(session_id).await;
            }

            Command::GetOpenAiCodexAuthStatus { response } => {
                response
                    .send(map_openai_codex_auth_status(
                        self.openai_codex_auth.get_status().await,
                    ))
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::StartOpenAiCodexBrowserLogin { response } => {
                self.openai_codex_auth.start_browser_login().await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::StartOpenAiCodexDeviceCodeLogin { response } => {
                self.openai_codex_auth.start_device_code_login().await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::CancelOpenAiCodexLogin { response } => {
                self.openai_codex_auth.cancel_login().await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::LogoutOpenAiCodexAuth { response } => {
                self.openai_codex_auth.logout().await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
        }

        Ok(())
    }

    async fn handle_send_message(
        &self,
        session_id: String,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
    ) {
        let stream_request = {
            let mut agent = self.agent_manager.lock().await;
            match agent
                .prepare_start_stream(&session_id, message, model_id, provider_id)
                .await
            {
                Ok(result) => Some((agent.cloned_provider_manager(), result)),
                Err(error) => {
                    self.send_event(Event::Error(error.to_string()));
                    None
                }
            }
        };

        let Some((providers, request)) = stream_request else {
            return;
        };

        self.spawn_stream_task(session_id, providers, request).await;
    }

    async fn handle_execute_tools(&self, session_id: String) {
        let event_callback = self.event_callback.clone();
        let agent_manager = self.agent_manager.clone();
        let command_tx = self.command_tx.clone();
        let active_streams = self.active_streams.clone();

        tokio::spawn(async move {
            let executions = {
                let mut agent = agent_manager.lock().await;
                agent.take_ready_tool_executions(&session_id)
            };

            let results = execute_tool_requests(executions).await;

            for result in &results {
                let success = result.output.get("error").is_none();
                let output = serde_json::to_string(&result.output).unwrap_or_default();

                event_callback.on_event(Event::ToolResultReady {
                    session_id: session_id.clone(),
                    call_id: result.call_id.to_string(),
                    tool_id: result.tool_id.to_string(),
                    success,
                    output,
                    denied: result.permission_denied,
                });
            }

            // Add results to history
            {
                let mut agent = agent_manager.lock().await;
                let _ = agent
                    .add_tool_results_to_history(&session_id, results)
                    .await;
            }

            tracing::debug!("Emitting HistoryUpdated event after tool results");
            event_callback.on_event(Event::HistoryUpdated {
                session_id: session_id.clone(),
            });

            let has_pending_tools = { agent_manager.lock().await.has_pending_tools(&session_id) };
            if !has_pending_tools {
                // Start continuation stream
                Self::start_continuation(
                    session_id,
                    agent_manager,
                    event_callback,
                    command_tx,
                    active_streams,
                )
                .await;
            }
        });
    }

    fn start_continuation(
        session_id: String,
        agent_manager: Arc<Mutex<AgentManager>>,
        event_callback: Arc<dyn EventCallback>,
        command_tx: mpsc::Sender<Command>,
        active_streams: Arc<Mutex<HashMap<String, ActiveStream>>>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            let continuation = {
                let mut agent = agent_manager.lock().await;
                match agent.prepare_continuation_stream(&session_id).await {
                    Ok(result) => {
                        Ok(result.map(|request| (agent.cloned_provider_manager(), request)))
                    }
                    Err(error) => Err(error),
                }
            };

            let ((providers, request), msg_id) = match continuation {
                Ok(Some((providers, request))) => {
                    let msg_id = request.message_id.clone();
                    ((providers, request), msg_id)
                }
                Ok(None) => return,
                Err(error) => {
                    {
                        let mut agent = agent_manager.lock().await;
                        agent.clear_active_turn(&session_id);
                    }
                    event_callback.on_event(Event::HistoryUpdated {
                        session_id: session_id.clone(),
                    });
                    event_callback.on_event(Event::ContinuationFailed {
                        session_id,
                        error: error.to_string(),
                    });
                    return;
                }
            };

            let start_gate = Arc::new(tokio::sync::Notify::new());
            let request_message_id = msg_id.clone();
            let request_session_id = session_id.clone();
            let task_active_streams = active_streams.clone();
            let task = tokio::spawn({
                let start_gate = start_gate.clone();
                async move {
                    start_gate.notified().await;
                    let result = Self::drive_stream(
                        request_session_id.clone(),
                        request,
                        providers,
                        agent_manager.clone(),
                        event_callback.clone(),
                    )
                    .await;

                    let stream_was_active = Self::clear_active_stream(
                        &task_active_streams,
                        &request_session_id,
                        &request_message_id,
                    )
                    .await;
                    if !stream_was_active {
                        return;
                    }

                    match result {
                        StreamDriveResult::Completed {
                            session_id: _completed_session,
                            content,
                        } => {
                            let completed_session = {
                                let agent = agent_manager.lock().await;
                                agent.complete_message(&request_message_id).await
                            };
                            let Ok(Some(completed_session)) = completed_session else {
                                return;
                            };

                            event_callback.on_event(Event::StreamComplete {
                                session_id: completed_session.clone(),
                                message_id: request_message_id.to_string(),
                            });
                            event_callback.on_event(Event::HistoryUpdated {
                                session_id: completed_session.clone(),
                            });
                            Self::process_completed_stream_output(
                                completed_session,
                                content,
                                agent_manager,
                                event_callback,
                                command_tx,
                                task_active_streams,
                            )
                            .await;
                        }
                        StreamDriveResult::FailedToStart { error } => {
                            {
                                let mut agent = agent_manager.lock().await;
                                let _ = agent.abort_streaming_message(&request_message_id).await;
                                agent.clear_active_turn(&request_session_id);
                            }
                            event_callback.on_event(Event::HistoryUpdated {
                                session_id: request_session_id.clone(),
                            });
                            event_callback.on_event(Event::ContinuationFailed {
                                session_id: request_session_id,
                                error,
                            });
                        }
                        StreamDriveResult::FailedDuringStream { error } => {
                            {
                                let mut agent = agent_manager.lock().await;
                                let _ = agent.abort_streaming_message(&request_message_id).await;
                                agent.clear_active_turn(&request_session_id);
                            }
                            tracing::error!("Continuation stream error: {}", error);
                            event_callback.on_event(Event::StreamError {
                                session_id: request_session_id,
                                message_id: request_message_id.to_string(),
                                error,
                            });
                        }
                        StreamDriveResult::Stopped => {}
                    }
                }
            });

            let previous = active_streams.lock().await.insert(
                session_id,
                ActiveStream {
                    message_id: msg_id,
                    abort_handle: task.abort_handle(),
                },
            );
            if let Some(previous) = previous {
                previous.abort_handle.abort();
            }
            start_gate.notify_one();
        })
    }

    async fn spawn_stream_task(
        &self,
        session_id: String,
        providers: ProviderManager,
        request: PendingStreamRequest,
    ) {
        let event_callback = self.event_callback.clone();
        let agent_manager = self.agent_manager.clone();
        let command_tx = self.command_tx.clone();
        let active_streams = self.active_streams.clone();
        let msg_id = request.message_id.clone();
        let start_gate = Arc::new(tokio::sync::Notify::new());
        let request_session_id = session_id.clone();
        let request_message_id = msg_id.clone();
        let task_active_streams = active_streams.clone();

        let task = tokio::spawn({
            let start_gate = start_gate.clone();
            async move {
                start_gate.notified().await;
                let result = Self::drive_stream(
                    request_session_id.clone(),
                    request,
                    providers,
                    agent_manager.clone(),
                    event_callback.clone(),
                )
                .await;

                let stream_was_active = Self::clear_active_stream(
                    &task_active_streams,
                    &request_session_id,
                    &request_message_id,
                )
                .await;
                if !stream_was_active {
                    return;
                }

                match result {
                    StreamDriveResult::Completed {
                        session_id: _completed_session,
                        content,
                    } => {
                        let completed_session = {
                            let agent = agent_manager.lock().await;
                            agent.complete_message(&request_message_id).await
                        };
                        let Ok(Some(completed_session)) = completed_session else {
                            return;
                        };

                        event_callback.on_event(Event::StreamComplete {
                            session_id: completed_session.clone(),
                            message_id: request_message_id.to_string(),
                        });
                        event_callback.on_event(Event::HistoryUpdated {
                            session_id: completed_session.clone(),
                        });
                        Self::process_completed_stream_output(
                            completed_session,
                            content,
                            agent_manager,
                            event_callback,
                            command_tx,
                            task_active_streams,
                        )
                        .await;
                    }
                    StreamDriveResult::FailedToStart { error } => {
                        {
                            let mut agent = agent_manager.lock().await;
                            let _ = agent.abort_streaming_message(&request_message_id).await;
                            agent.clear_active_turn(&request_session_id);
                        }
                        event_callback.on_event(Event::Error(error));
                    }
                    StreamDriveResult::FailedDuringStream { error } => {
                        {
                            let mut agent = agent_manager.lock().await;
                            let _ = agent.abort_streaming_message(&request_message_id).await;
                            agent.clear_active_turn(&request_session_id);
                        }
                        event_callback.on_event(Event::StreamError {
                            session_id: request_session_id,
                            message_id: request_message_id.to_string(),
                            error,
                        });
                    }
                    StreamDriveResult::Stopped => {}
                }
            }
        });

        let previous = self.active_streams.lock().await.insert(
            session_id,
            ActiveStream {
                message_id: msg_id,
                abort_handle: task.abort_handle(),
            },
        );
        if let Some(previous) = previous {
            previous.abort_handle.abort();
        }
        start_gate.notify_one();
    }

    async fn drive_stream(
        session_id: String,
        request: PendingStreamRequest,
        providers: ProviderManager,
        agent_manager: Arc<Mutex<AgentManager>>,
        event_callback: Arc<dyn EventCallback>,
    ) -> StreamDriveResult {
        let msg_id = request.message_id.clone();
        let mut stream = match providers
            .generate_reply_stream(
                request.provider_id,
                &request.model_id,
                request.provider_messages,
            )
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                return StreamDriveResult::FailedToStart {
                    error: error.to_string(),
                };
            }
        };

        event_callback.on_event(Event::StreamStart {
            session_id: session_id.clone(),
            message_id: msg_id.to_string(),
        });

        let mut content = String::new();

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    content.push_str(&chunk);
                    {
                        let agent = agent_manager.lock().await;
                        if !agent.append_chunk(&msg_id, &chunk).await {
                            return StreamDriveResult::Stopped;
                        }
                    }
                    event_callback.on_event(Event::StreamChunk {
                        session_id: session_id.clone(),
                        message_id: msg_id.to_string(),
                        chunk,
                    });
                }
                Err(error) => {
                    return StreamDriveResult::FailedDuringStream {
                        error: error.to_string(),
                    };
                }
            }
        }

        tracing::debug!("Full content length: {}", content.len());

        StreamDriveResult::Completed {
            session_id,
            content,
        }
    }

    async fn process_completed_stream_output(
        completed_session: String,
        content: String,
        agent_manager: Arc<Mutex<AgentManager>>,
        event_callback: Arc<dyn EventCallback>,
        command_tx: mpsc::Sender<Command>,
        active_streams: Arc<Mutex<HashMap<String, ActiveStream>>>,
    ) {
        let (tool_calls, failed) = {
            let mut agent = agent_manager.lock().await;
            match agent
                .parse_tool_calls_from_content(&completed_session, &content)
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    event_callback.on_event(Event::Error(error.to_string()));
                    return;
                }
            }
        };

        tracing::debug!(
            "Found {} tool calls, {} failed",
            tool_calls.len(),
            failed.len()
        );

        if !failed.is_empty() {
            tracing::warn!("Failed tool calls found, adding to history");
            {
                let mut agent = agent_manager.lock().await;
                let _ = agent
                    .add_parse_failures_to_history(&completed_session, failed)
                    .await;
            }
            event_callback.on_event(Event::HistoryUpdated {
                session_id: completed_session.clone(),
            });
            Self::start_continuation(
                completed_session,
                agent_manager,
                event_callback,
                command_tx,
                active_streams,
            )
            .await;
            return;
        }

        let had_tool_calls = !tool_calls.is_empty();
        let mut has_auto_approved_tools = false;

        for tool_call in tool_calls {
            let args_json = {
                let agent = agent_manager.lock().await;
                agent
                    .get_pending_tool_args(&completed_session, &tool_call.call_id)
                    .map(|args| serde_json::to_string(&args).unwrap_or_default())
                    .unwrap_or_default()
            };

            if tool_call.requires_confirmation {
                tracing::debug!(
                    "Emitting ToolCallDetected: {} - {}",
                    tool_call.tool_id,
                    tool_call.description
                );
                event_callback.on_event(Event::ToolCallDetected {
                    session_id: completed_session.clone(),
                    call_id: tool_call.call_id.to_string(),
                    tool_id: tool_call.tool_id,
                    args: args_json,
                    description: tool_call.description,
                    risk_level: tool_call.assessment.risk.as_str().to_string(),
                    reasons: tool_call.assessment.reasons,
                    queue_order: tool_call.queue_order,
                });
            } else {
                has_auto_approved_tools = true;
            }
        }

        if has_auto_approved_tools {
            let _ = command_tx
                .send(Command::ExecuteApprovedTools {
                    session_id: completed_session,
                })
                .await;
        } else if !had_tool_calls {
            {
                let mut agent = agent_manager.lock().await;
                agent.clear_active_turn(&completed_session);
            }
            event_callback.on_event(Event::HistoryUpdated {
                session_id: completed_session,
            });
        }
    }

    async fn clear_active_stream(
        active_streams: &Arc<Mutex<HashMap<String, ActiveStream>>>,
        session_id: &str,
        message_id: &MessageId,
    ) -> bool {
        let mut active_streams = active_streams.lock().await;
        let should_remove = active_streams
            .get(session_id)
            .is_some_and(|stream| &stream.message_id == message_id);
        if should_remove {
            active_streams.remove(session_id);
        }
        should_remove
    }

    async fn take_active_stream(&self, session_id: &str) -> Option<ActiveStream> {
        self.active_streams.lock().await.remove(session_id)
    }

    async fn cancel_stream(&self, session_id: String) -> Result<bool> {
        let Some(active_stream) = self.take_active_stream(&session_id).await else {
            return Ok(false);
        };

        active_stream.abort_handle.abort();

        let cancelled_stream = {
            let mut agent = self.agent_manager.lock().await;
            let cancelled = agent
                .cancel_streaming_message(&active_stream.message_id)
                .await?;
            agent.clear_active_turn(&session_id);
            cancelled
        };
        let Some(cancelled_stream) = cancelled_stream else {
            return Ok(false);
        };

        self.send_event(Event::StreamCancelled {
            session_id: cancelled_stream.session_id.clone(),
            message_id: cancelled_stream.message_id.to_string(),
        });
        self.send_event(Event::HistoryUpdated {
            session_id: cancelled_stream.session_id,
        });
        Ok(true)
    }

    fn spawn_config_watcher(&self) {
        let command_tx = self.command_tx.clone();
        let callback = self.event_callback.clone();

        tokio::spawn(async move {
            let config_loc = match settings_path() {
                Ok(path) => path,
                Err(error) => {
                    callback.on_event(Event::Error(format!("Config path error: {error}")));
                    return;
                }
            };
            let config_dir = match config_loc.parent() {
                Some(path) => path.to_path_buf(),
                None => {
                    callback.on_event(Event::Error(String::from("Config path has no parent")));
                    return;
                }
            };
            if let Err(error) = std::fs::create_dir_all(&config_dir) {
                callback.on_event(Event::Error(format!(
                    "Failed to create config directory {}: {error}",
                    config_dir.display()
                )));
                return;
            }

            let (tx, rx) = std::sync::mpsc::channel();
            let mut watcher = match notify::recommended_watcher(tx) {
                Ok(watcher) => watcher,
                Err(error) => {
                    callback.on_event(Event::Error(format!(
                        "Failed to create config watcher: {error}"
                    )));
                    return;
                }
            };

            if let Err(error) = watcher.watch(&config_dir, RecursiveMode::NonRecursive) {
                callback.on_event(Event::Error(format!(
                    "Failed to watch config directory {}: {error}",
                    config_dir.display()
                )));
                return;
            }

            for res in rx {
                match res {
                    Ok(event) => {
                        if event.kind.is_access() {
                            continue;
                        }
                        if !event.paths.iter().any(|path| path == &config_loc) {
                            continue;
                        }
                        let _ = command_tx.send(Command::LoadConfig).await;
                    }
                    Err(e) => {
                        callback.on_event(Event::Error(format!("Config watch error: {:?}", e)));
                    }
                }
            }
        });
    }

    async fn load_providers_config(&self) -> Result<()> {
        let config_loc = settings_path()?;
        let config = if !config_loc.exists() {
            ProviderManagerConfig {
                providers: Vec::new(),
                models: Vec::new(),
            }
        } else {
            let config_slice = tokio::fs::read(&config_loc).await?;
            toml::from_slice(&config_slice).wrap_err("Failed to parse providers.toml")?
        };

        self.agent_manager
            .lock()
            .await
            .set_providers(config, self.provider_registry.clone())
            .await?;

        Ok(())
    }

    async fn save_settings_document(&self, settings: SettingsDocument) -> Result<()> {
        let config_loc = settings_path()?;
        write_settings_document(&config_loc, &settings, &self.provider_registry).await?;
        self.load_providers_config().await?;
        tracing::info!("Loaded config");
        self.send_event(Event::ConfigLoaded);
        Ok(())
    }
}

fn canonicalize_workspace_dir(path: &str) -> Result<PathBuf> {
    let raw = PathBuf::from(path);
    if !raw.exists() {
        return Err(eyre!(
            "Workspace directory does not exist: {}",
            raw.display()
        ));
    }
    if !raw.is_dir() {
        return Err(eyre!(
            "Workspace path is not a directory: {}",
            raw.display()
        ));
    }

    Ok(raw.canonicalize().unwrap_or(raw))
}

fn settings_path() -> Result<std::path::PathBuf> {
    Ok(agent_state_root()?.join("providers.toml"))
}

fn map_openai_codex_auth_status(status: ProviderOpenAiCodexAuthStatus) -> OpenAiCodexAuthStatus {
    let state = match status.state {
        ProviderOpenAiCodexLoginState::SignedOut => OpenAiCodexLoginState::SignedOut,
        ProviderOpenAiCodexLoginState::BrowserPending(pending) => {
            OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                auth_url: pending.auth_url,
            })
        }
        ProviderOpenAiCodexLoginState::DeviceCodePending(pending) => {
            OpenAiCodexLoginState::DeviceCodePending(PendingDeviceCodeLogin {
                verification_url: pending.verification_url,
                user_code: pending.user_code,
            })
        }
        ProviderOpenAiCodexLoginState::Authenticated => OpenAiCodexLoginState::Authenticated,
    };

    OpenAiCodexAuthStatus {
        state,
        email: status.email,
        plan_type: status.plan_type,
        account_id: status.account_id,
        last_refresh_unix: status.last_refresh_unix,
        error: status.error,
    }
}

fn read_settings_document(
    path: &std::path::Path,
    registry: &ProviderRegistry,
) -> Result<SettingsDocument> {
    if !path.exists() {
        return Ok(SettingsDocument::default());
    }

    let config_slice = std::fs::read(path)?;
    let config: ProviderManagerConfig =
        toml::from_slice(&config_slice).wrap_err("Failed to parse providers.toml")?;
    settings_from_provider_config(config, registry)
}

async fn write_settings_document(
    path: &std::path::Path,
    settings: &SettingsDocument,
    registry: &ProviderRegistry,
) -> Result<()> {
    let errors = validate_settings(settings, registry);
    if !errors.is_empty() {
        let message = errors
            .into_iter()
            .map(|error| format!("{}: {}", error.field, error.message))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(eyre!(message));
    }

    let config = provider_config_from_settings(settings)?;
    let toml_string = toml::to_string_pretty(&config)?;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let temp_path = path.with_extension("toml.tmp");
    tokio::fs::write(&temp_path, toml_string).await?;
    tokio::fs::rename(&temp_path, path).await?;
    Ok(())
}

fn settings_from_provider_config(
    config: ProviderManagerConfig,
    registry: &ProviderRegistry,
) -> Result<SettingsDocument> {
    let providers = config
        .providers
        .into_iter()
        .map(provider_settings_from_config)
        .collect::<Result<Vec<_>>>()?;
    let models = config
        .models
        .into_iter()
        .map(model_settings_from_config)
        .collect::<Result<Vec<_>>>()?;

    let settings = SettingsDocument { providers, models };
    let errors = validate_settings(&settings, registry);
    if errors.is_empty() {
        Ok(settings)
    } else {
        Err(eyre!(format_settings_errors(errors)))
    }
}

fn provider_settings_from_config(config: ProviderConfig) -> Result<ProviderSettings> {
    Ok(ProviderSettings {
        id: config.id.to_string(),
        type_id: config.type_id,
        values: config
            .config
            .into_iter()
            .map(|(key, value)| FieldValueEntry { key, value })
            .collect(),
    })
}

fn model_settings_from_config(config: ModelConfig) -> Result<ModelSettings> {
    Ok(ModelSettings {
        id: config.id.to_string(),
        provider_id: config.provider_id.to_string(),
        values: config
            .config
            .into_iter()
            .map(|(key, value)| FieldValueEntry { key, value })
            .collect(),
    })
}

fn provider_config_from_settings(settings: &SettingsDocument) -> Result<ProviderManagerConfig> {
    let providers = settings
        .providers
        .iter()
        .map(provider_config_entry_from_settings)
        .collect::<Result<Vec<_>>>()?;
    let models = settings
        .models
        .iter()
        .map(model_config_entry_from_settings)
        .collect::<Result<Vec<_>>>()?;
    Ok(ProviderManagerConfig { providers, models })
}

fn provider_config_entry_from_settings(settings: &ProviderSettings) -> Result<ProviderConfig> {
    Ok(ProviderConfig {
        id: ProviderId::new(settings.id.trim().to_string()),
        type_id: settings.type_id.trim().to_string(),
        config: values_to_dynamic_config(&settings.values),
    })
}

fn model_config_entry_from_settings(settings: &ModelSettings) -> Result<ModelConfig> {
    Ok(ModelConfig {
        id: ModelId::new(settings.id.trim().to_string()),
        provider_id: ProviderId::new(settings.provider_id.trim().to_string()),
        config: values_to_dynamic_config(&settings.values),
    })
}

fn validate_settings(
    settings: &SettingsDocument,
    registry: &ProviderRegistry,
) -> Vec<SettingsValidationError> {
    let mut errors = Vec::new();
    let mut provider_ids = std::collections::BTreeSet::new();
    let mut provider_types = BTreeMap::new();

    for (index, provider) in settings.providers.iter().enumerate() {
        let field_prefix = format!("providers[{index}]");
        let id = provider.id.trim();
        if id.is_empty() {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.id"),
                message: String::from("Provider ID is required"),
            });
        } else if !provider_ids.insert(id.to_string()) {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.id"),
                message: String::from("Provider ID must be unique"),
            });
        }
        let type_id = provider.type_id.trim();
        if type_id.is_empty() {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.type_id"),
                message: String::from("Provider type is required"),
            });
            continue;
        }
        if !registry.has_factory(type_id) {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.type_id"),
                message: format!("Unsupported provider type: {type_id}"),
            });
            continue;
        }
        provider_types.insert(id.to_string(), type_id.to_string());
        for error in registry
            .validate_provider_config(type_id, &values_to_dynamic_config(&provider.values))
            .unwrap_or_default()
        {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.{}", error.field),
                message: error.message,
            });
        }
    }

    for (index, model) in settings.models.iter().enumerate() {
        let field_prefix = format!("models[{index}]");
        if model.id.trim().is_empty() {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.id"),
                message: String::from("Model ID is required"),
            });
        }
        if model.provider_id.trim().is_empty() {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.provider_id"),
                message: String::from("Provider ID is required"),
            });
            continue;
        }
        let Some(provider_type) = provider_types.get(model.provider_id.trim()) else {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.provider_id"),
                message: String::from("Model must reference an existing provider"),
            });
            continue;
        };
        for error in registry
            .validate_model_config(provider_type, &values_to_dynamic_config(&model.values))
            .unwrap_or_default()
        {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.{}", error.field),
                message: error.message,
            });
        }
    }

    errors
}

fn values_to_dynamic_config(values: &[FieldValueEntry]) -> DynamicConfig {
    values
        .iter()
        .map(|entry| (entry.key.clone(), entry.value.clone()))
        .collect()
}

fn format_settings_errors(errors: Vec<SettingsValidationError>) -> String {
    errors
        .into_iter()
        .map(|error| format!("{}: {}", error.field, error.message))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;
    use agent::AgentManager;
    use async_trait::async_trait;
    use color_eyre::eyre::{Result, eyre};
    use futures::stream::{self, BoxStream};
    use persistence::{FileMessageStore, FileSessionStore};
    use serde::Deserialize;
    use tool_core::{ToolContext, ToolManager, ToolOutput, TypedTool};
    use tool_edit_file::EditFileTool;
    use types::{
        ChatMessage, ChatRole, ExecutionPolicy, MessageStatus, ModelId, ProviderId, RiskLevel,
        ToolCallAssessment,
    };

    #[derive(Clone, Debug)]
    struct ScriptedChunk {
        text: String,
        gate: Option<Arc<tokio::sync::Notify>>,
    }

    impl ScriptedChunk {
        fn plain(text: impl Into<String>) -> Self {
            Self {
                text: text.into(),
                gate: None,
            }
        }

        fn gated(text: impl Into<String>, gate: Arc<tokio::sync::Notify>) -> Self {
            Self {
                text: text.into(),
                gate: Some(gate),
            }
        }
    }

    #[derive(Clone, Deserialize)]
    struct ValueArgs {
        value: String,
    }

    #[derive(Clone, Deserialize)]
    struct NoopArgs {}

    struct ScriptedProvider {
        id: ProviderId,
        scripts: StdMutex<VecDeque<Vec<ScriptedChunk>>>,
    }

    #[async_trait]
    impl provider_core::Provider for ScriptedProvider {
        fn get_provider_id(&self) -> ProviderId {
            self.id.clone()
        }

        async fn list_models(&self) -> Vec<provider_core::Model> {
            vec![provider_core::Model {
                id: ModelId::new("mock-model"),
                name: String::from("Mock Model"),
                max_context: None,
            }]
        }

        async fn cache_models(&self) -> Result<()> {
            Ok(())
        }

        async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
            Ok(())
        }

        async fn generate_reply(
            &self,
            _model_id: &ModelId,
            _messages: Vec<ChatMessage>,
        ) -> Result<ChatMessage> {
            Ok(ChatMessage {
                role: ChatRole::Assistant,
                content: String::from("unused non-streaming reply"),
            })
        }

        async fn generate_reply_stream(
            &self,
            _model_id: &ModelId,
            _messages: Vec<ChatMessage>,
        ) -> Result<BoxStream<'static, Result<String>>> {
            let script = self
                .scripts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .ok_or_else(|| eyre!("No scripted stream remaining"))?;

            Ok(Box::pin(stream::unfold(
                (script, 0usize),
                |(script, index)| async move {
                    if index >= script.len() {
                        return None;
                    }

                    let chunk = script[index].clone();
                    if let Some(gate) = chunk.gate {
                        gate.notified().await;
                    }

                    Some((Ok(chunk.text), (script, index + 1)))
                },
            )))
        }
    }

    #[derive(Clone, Copy)]
    struct ApprovalTool;

    #[async_trait]
    impl TypedTool for ApprovalTool {
        type Args = ValueArgs;

        fn name(&self) -> &'static str {
            "mock_tool"
        }

        fn schema(&self) -> &'static str {
            "mock_tool(value: string)"
        }

        fn assess(&self, args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
            ToolCallAssessment {
                risk: RiskLevel::UndoableWorkspaceWrite,
                policy: ExecutionPolicy::AlwaysAsk,
                reasons: vec![format!("mock_tool requires approval for {:?}", args.value)],
            }
        }

        async fn call(&self, args: Self::Args, _ctx: &ToolContext<'_>) -> ToolOutput {
            ToolOutput::success(serde_json::json!({
                "tool": "mock_tool",
                "value": args.value,
            }))
        }

        fn describe(&self, args: &Self::Args) -> String {
            format!("Mock tool for {}", args.value)
        }
    }

    #[derive(Clone)]
    struct BlockingApprovalTool {
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        fail_message: Option<String>,
    }

    #[async_trait]
    impl TypedTool for BlockingApprovalTool {
        type Args = ValueArgs;

        fn name(&self) -> &'static str {
            "blocking_tool"
        }

        fn schema(&self) -> &'static str {
            "blocking_tool(value: string)"
        }

        fn assess(&self, args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
            ToolCallAssessment {
                risk: RiskLevel::UndoableWorkspaceWrite,
                policy: ExecutionPolicy::AlwaysAsk,
                reasons: vec![format!(
                    "blocking_tool requires approval for {:?}",
                    args.value
                )],
            }
        }

        async fn call(&self, args: Self::Args, _ctx: &ToolContext<'_>) -> ToolOutput {
            self.started.notify_waiters();
            self.release.notified().await;

            if let Some(message) = &self.fail_message {
                ToolOutput::error(message.clone())
            } else {
                ToolOutput::success(serde_json::json!({
                    "tool": "blocking_tool",
                    "value": args.value,
                }))
            }
        }

        fn describe(&self, args: &Self::Args) -> String {
            format!("Blocking tool for {}", args.value)
        }
    }

    struct BlockingStartProvider {
        id: ProviderId,
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[derive(Clone, Copy)]
    struct NoopTool;

    #[async_trait]
    impl TypedTool for NoopTool {
        type Args = NoopArgs;

        fn name(&self) -> &'static str {
            "noop_tool"
        }

        fn schema(&self) -> &'static str {
            "noop_tool()"
        }

        async fn call(&self, _args: Self::Args, _ctx: &ToolContext<'_>) -> ToolOutput {
            ToolOutput::success(serde_json::json!({ "ok": true }))
        }
    }

    #[async_trait]
    impl provider_core::Provider for BlockingStartProvider {
        fn get_provider_id(&self) -> ProviderId {
            self.id.clone()
        }

        async fn list_models(&self) -> Vec<provider_core::Model> {
            vec![provider_core::Model {
                id: ModelId::new("mock-model"),
                name: String::from("Mock Model"),
                max_context: None,
            }]
        }

        async fn cache_models(&self) -> Result<()> {
            Ok(())
        }

        async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
            Ok(())
        }

        async fn generate_reply(
            &self,
            _model_id: &ModelId,
            _messages: Vec<ChatMessage>,
        ) -> Result<ChatMessage> {
            Ok(ChatMessage {
                role: ChatRole::Assistant,
                content: String::from("unused non-streaming reply"),
            })
        }

        async fn generate_reply_stream(
            &self,
            _model_id: &ModelId,
            _messages: Vec<ChatMessage>,
        ) -> Result<BoxStream<'static, Result<String>>> {
            self.started.notify_waiters();
            self.release.notified().await;
            Ok(Box::pin(stream::once(async {
                Ok(String::from("provider started"))
            })))
        }
    }

    #[derive(Clone, Default)]
    struct EventCollector {
        events: Arc<StdMutex<Vec<Event>>>,
    }

    impl EventCollector {
        fn snapshot(&self) -> Vec<Event> {
            self.events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        async fn wait_for<F>(&self, description: &str, predicate: F) -> Vec<Event>
        where
            F: Fn(&[Event]) -> bool,
        {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                let snapshot = self.snapshot();
                if predicate(&snapshot) {
                    return snapshot;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "Timed out waiting for {description}. Events so far: {snapshot:#?}"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    impl EventCallback for EventCollector {
        fn on_event(&self, event: Event) {
            self.events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(event);
        }
    }

    struct RuntimeTestHarness {
        handle: RuntimeHandle,
        events: EventCollector,
        runtime_task: tokio::task::JoinHandle<()>,
        data_dir: PathBuf,
    }

    impl RuntimeTestHarness {
        async fn new(scripts: Vec<Vec<ScriptedChunk>>) -> Self {
            Self::new_with_tools(scripts, |_| {}).await
        }

        async fn new_with_tools<F>(scripts: Vec<Vec<ScriptedChunk>>, configure_tools: F) -> Self
        where
            F: FnOnce(&mut ToolManager),
        {
            let mut providers = ProviderManager::new();
            providers.register_provider(
                ProviderId::new("mock"),
                Box::new(ScriptedProvider {
                    id: ProviderId::new("mock"),
                    scripts: StdMutex::new(scripts.into()),
                }),
            );

            let mut tools = ToolManager::new();
            tools.register_tool(ApprovalTool);
            configure_tools(&mut tools);

            Self::new_with_parts(providers, tools).await
        }

        async fn new_with_parts(providers: ProviderManager, mut tools: ToolManager) -> Self {
            if tools.list_tools().is_empty() {
                tools.register_tool(ApprovalTool);
            }
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let pid = std::process::id();
            let data_dir = std::env::temp_dir().join(format!("agent-runtime-test-{pid}-{nanos}"));
            tokio::fs::create_dir_all(&data_dir)
                .await
                .expect("create temp runtime dir");

            let workspace_dir = data_dir.join("workspace");
            tokio::fs::create_dir_all(&workspace_dir)
                .await
                .expect("create temp workspace dir");
            tools.register_tool(NoopTool);
            let profile_dir = workspace_dir.join(".agent");
            tokio::fs::create_dir_all(&profile_dir)
                .await
                .expect("create temp profile dir");
            let tool_ids = tools
                .list_tools()
                .into_iter()
                .map(|tool_id| format!("\"{}\"", tool_id))
                .collect::<Vec<_>>()
                .join(", ");
            let profile_doc = format!(
                "[[profiles]]\n\
id = \"test-profile\"\n\
display_name = \"Test Profile\"\n\
description = \"Runtime test profile\"\n\
system_prompt = \"Runtime test profile\"\n\
tools = [{tool_ids}]\n\
default_risk_level = \"undoable_workspace_write\"\n"
            );
            tokio::fs::write(profile_dir.join("agents.toml"), profile_doc)
                .await
                .expect("write test profile config");

            let message_store = Arc::new(FileMessageStore::new(&data_dir));
            let session_store = Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));

            let agent_manager = Arc::new(Mutex::new(AgentManager::new(
                providers,
                tools,
                workspace_dir,
                message_store,
                session_store,
            )));

            let events = EventCollector::default();
            let (command_tx, mut command_rx) = mpsc::channel(32);
            let handle = RuntimeHandle {
                command_tx: command_tx.clone(),
            };

            let openai_codex_auth =
                Arc::new(OpenAiCodexAuthController::new().expect("openai auth controller"));
            let runtime = RuntimeInner {
                event_callback: Arc::new(events.clone()),
                command_tx,
                agent_manager,
                provider_registry: build_provider_registry(openai_codex_auth.clone())
                    .expect("provider registry"),
                active_streams: Arc::new(Mutex::new(HashMap::new())),
                openai_codex_auth,
            };

            let runtime_task = tokio::spawn(async move {
                while let Some(command) = command_rx.recv().await {
                    runtime
                        .handle_command(command)
                        .await
                        .expect("runtime command should succeed");
                }
            });

            Self {
                handle,
                events,
                runtime_task,
                data_dir,
            }
        }

        async fn shutdown(self) {
            drop(self.handle);
            self.runtime_task.abort();
            let _ = self.runtime_task.await;
            let _ = tokio::fs::remove_dir_all(self.data_dir).await;
        }
    }

    async fn create_session_with_profile(
        handle: &RuntimeHandle,
        profile_id: &str,
    ) -> Result<String> {
        let session_id = handle.create_session().await?;
        handle
            .set_session_profile(session_id.clone(), profile_id.to_string())
            .await?;
        Ok(session_id)
    }

    fn stream_complete_for(events: &[Event], session_id: &str) -> usize {
        events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    Event::StreamComplete {
                        session_id: event_session,
                        ..
                    } if event_session == session_id
                )
            })
            .expect("stream complete event should exist")
    }

    #[tokio::test]
    async fn background_session_stream_continues_after_switch() -> Result<()> {
        let gate = Arc::new(tokio::sync::Notify::new());
        let harness = RuntimeTestHarness::new(vec![
            vec![
                ScriptedChunk::plain("session-a chunk 1 "),
                ScriptedChunk::gated("session-a chunk 2", gate.clone()),
            ],
            vec![ScriptedChunk::plain("session-b complete")],
        ])
        .await;

        let session_a = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_a.clone(),
                String::from("start session a"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("session A first chunk", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamChunk {
                            session_id,
                            chunk,
                            ..
                        } if session_id == &session_a && chunk == "session-a chunk 1 "
                    )
                })
            })
            .await;

        let session_b = create_session_with_profile(&harness.handle, "test-profile").await?;
        assert!(harness.handle.load_session(session_b.clone()).await?);
        harness
            .handle
            .send_message(
                session_b.clone(),
                String::from("start session b"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let events = harness
            .events
            .wait_for("session B completion", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamComplete {
                            session_id,
                            ..
                        } if session_id == &session_b
                    )
                })
            })
            .await;

        assert!(
            !events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamComplete {
                        session_id,
                        ..
                    } if session_id == &session_a
                )
            }),
            "session A should still be streaming while session B completes"
        );

        gate.notify_one();

        let events = harness
            .events
            .wait_for("session A completion", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamComplete {
                            session_id,
                            ..
                        } if session_id == &session_a
                    )
                })
            })
            .await;

        assert!(
            stream_complete_for(&events, &session_b) < stream_complete_for(&events, &session_a)
        );

        let history_a = harness.handle.get_chat_history(session_a.clone()).await?;
        let history_b = harness.handle.get_chat_history(session_b.clone()).await?;
        assert_eq!(history_a.len(), 2);
        assert_eq!(history_b.len(), 2);
        assert!(
            history_a
                .values()
                .any(|message| message.content == "session-a chunk 1 session-a chunk 2")
        );
        assert!(
            history_b
                .values()
                .any(|message| message.content == "session-b complete")
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn background_session_tool_approval_and_continuation_work_after_switch() -> Result<()> {
        let harness = RuntimeTestHarness::new(vec![
            vec![
                ScriptedChunk::plain("before tool\n"),
                ScriptedChunk::plain(
                    "<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>",
                ),
            ],
            vec![ScriptedChunk::plain("session-b reply")],
            vec![ScriptedChunk::plain("continuation complete")],
        ])
        .await;

        let session_a = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_a.clone(),
                String::from("start session a"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let events = harness
            .events
            .wait_for("session A tool detection", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolCallDetected {
                            session_id,
                            tool_id,
                            ..
                        } if session_id == &session_a && tool_id == "mock_tool"
                    )
                })
            })
            .await;

        let call_id = events
            .iter()
            .find_map(|event| match event {
                Event::ToolCallDetected {
                    session_id,
                    call_id,
                    tool_id,
                    ..
                } if session_id == &session_a && tool_id == "mock_tool" => Some(call_id.clone()),
                _ => None,
            })
            .expect("tool call id should exist");

        let session_b = create_session_with_profile(&harness.handle, "test-profile").await?;
        assert!(harness.handle.load_session(session_b.clone()).await?);
        harness
            .handle
            .send_message(
                session_b.clone(),
                String::from("start session b"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("session B completion", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamComplete {
                            session_id,
                            ..
                        } if session_id == &session_b
                    )
                })
            })
            .await;

        let session_b_tip_before = harness.handle.get_tip(session_b.clone()).await?;

        harness
            .handle
            .approve_tool(session_a.clone(), call_id.clone())
            .await?;
        harness
            .handle
            .execute_approved_tools(session_a.clone())
            .await?;

        harness
            .events
            .wait_for("session A tool result and continuation", |events| {
                let tool_result_ready = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id,
                            call_id: event_call_id,
                            tool_id,
                            denied,
                            ..
                        } if session_id == &session_a
                            && event_call_id == &call_id
                            && tool_id == "mock_tool"
                            && !denied
                    )
                });
                let continuation_completed = events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::StreamComplete {
                                session_id,
                                ..
                            } if session_id == &session_a
                        )
                    })
                    .count()
                    >= 2;
                tool_result_ready && continuation_completed
            })
            .await;

        let history_a = harness.handle.get_chat_history(session_a.clone()).await?;
        let history_b = harness.handle.get_chat_history(session_b.clone()).await?;
        let session_b_tip_after = harness.handle.get_tip(session_b.clone()).await?;

        assert_eq!(session_b_tip_before, session_b_tip_after);
        assert_eq!(history_b.len(), 2);
        assert!(
            history_b
                .values()
                .any(|message| message.content == "session-b reply")
        );
        assert!(
            history_a
                .values()
                .any(|message| message.content.contains("Tool 'mock_tool' result"))
        );
        assert!(
            history_a
                .values()
                .any(|message| message.content == "continuation complete")
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn session_operations_remain_responsive_while_tool_executes() -> Result<()> {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let harness = RuntimeTestHarness::new_with_tools(
            vec![vec![
                ScriptedChunk::plain("before tool\n"),
                ScriptedChunk::plain(
                    "<tool_call>\n\
tool: blocking_tool\n\
value: alpha\n\
</tool_call>",
                ),
            ]],
            {
                let started = started.clone();
                let release = release.clone();
                move |tools| {
                    tools.register_tool(BlockingApprovalTool {
                        started,
                        release,
                        fail_message: None,
                    });
                }
            },
        )
        .await;

        let session_a = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_a.clone(),
                String::from("start session a"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let events = harness
            .events
            .wait_for("blocking tool detection", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolCallDetected {
                            session_id,
                            tool_id,
                            ..
                        } if session_id == &session_a && tool_id == "blocking_tool"
                    )
                })
            })
            .await;

        let call_id = events
            .iter()
            .find_map(|event| match event {
                Event::ToolCallDetected {
                    session_id,
                    call_id,
                    tool_id,
                    ..
                } if session_id == &session_a && tool_id == "blocking_tool" => {
                    Some(call_id.clone())
                }
                _ => None,
            })
            .expect("tool call id should exist");

        let session_b = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .approve_tool(session_a.clone(), call_id.clone())
            .await?;
        harness
            .handle
            .execute_approved_tools(session_a.clone())
            .await?;
        started.notified().await;

        let load_result = tokio::time::timeout(
            Duration::from_millis(200),
            harness.handle.load_session(session_b.clone()),
        )
        .await;
        assert!(matches!(load_result, Ok(Ok(true))));

        let pending_tools_result = tokio::time::timeout(
            Duration::from_millis(200),
            harness.handle.get_pending_tools(session_b.clone()),
        )
        .await;
        assert!(matches!(pending_tools_result, Ok(Ok(tools)) if tools.is_empty()));

        release.notify_waiters();

        harness
            .events
            .wait_for("blocking tool result", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id,
                            call_id: event_call_id,
                            tool_id,
                            ..
                        } if session_id == &session_a
                            && event_call_id == &call_id
                            && tool_id == "blocking_tool"
                    )
                })
            })
            .await;

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn failed_tool_result_is_persisted_before_continuation_failure() -> Result<()> {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let harness = RuntimeTestHarness::new_with_tools(
            vec![vec![
                ScriptedChunk::plain("before tool\n"),
                ScriptedChunk::plain(
                    "<tool_call>\n\
tool: blocking_tool\n\
value: alpha\n\
</tool_call>",
                ),
            ]],
            {
                let started = started.clone();
                let release = release.clone();
                move |tools| {
                    tools.register_tool(BlockingApprovalTool {
                        started,
                        release,
                        fail_message: Some(String::from("tool exploded")),
                    });
                }
            },
        )
        .await;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("start session"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let events = harness
            .events
            .wait_for("failing tool detection", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolCallDetected {
                            session_id: event_session,
                            tool_id,
                            ..
                        } if event_session == &session_id && tool_id == "blocking_tool"
                    )
                })
            })
            .await;

        let call_id = events
            .iter()
            .find_map(|event| match event {
                Event::ToolCallDetected {
                    session_id: event_session,
                    call_id,
                    tool_id,
                    ..
                } if event_session == &session_id && tool_id == "blocking_tool" => {
                    Some(call_id.clone())
                }
                _ => None,
            })
            .expect("tool call id should exist");

        harness
            .handle
            .approve_tool(session_id.clone(), call_id.clone())
            .await?;
        harness
            .handle
            .execute_approved_tools(session_id.clone())
            .await?;
        started.notified().await;
        release.notify_waiters();

        harness
            .events
            .wait_for("continuation failure after tool error", |events| {
                let tool_result_ready = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            call_id: event_call_id,
                            tool_id,
                            success,
                            denied,
                            ..
                        } if event_session == &session_id
                            && event_call_id == &call_id
                            && tool_id == "blocking_tool"
                            && !success
                            && !denied
                    )
                });
                let continuation_failed = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ContinuationFailed {
                            session_id: event_session,
                            ..
                        } if event_session == &session_id
                    )
                });
                tool_result_ready && continuation_failed
            })
            .await;

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(history.values().any(|message| {
            message
                .content
                .contains("Tool 'blocking_tool' result:\n{\n  \"error\": \"tool exploded\"\n}")
        }));

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn session_operations_remain_responsive_while_provider_stream_starts() -> Result<()> {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut providers = ProviderManager::new();
        providers.register_provider(
            ProviderId::new("mock"),
            Box::new(BlockingStartProvider {
                id: ProviderId::new("mock"),
                started: started.clone(),
                release: release.clone(),
            }),
        );

        let harness = RuntimeTestHarness::new_with_parts(providers, ToolManager::new()).await;
        let session_a = create_session_with_profile(&harness.handle, "test-profile").await?;
        let session_b = create_session_with_profile(&harness.handle, "test-profile").await?;

        harness
            .handle
            .send_message(
                session_a.clone(),
                String::from("start session a"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;
        started.notified().await;

        let load_result = tokio::time::timeout(
            Duration::from_millis(200),
            harness.handle.load_session(session_b.clone()),
        )
        .await;
        assert!(matches!(load_result, Ok(Ok(true))));

        let tip_result = tokio::time::timeout(
            Duration::from_millis(200),
            harness.handle.get_tip(session_b.clone()),
        )
        .await;
        assert!(matches!(tip_result, Ok(Ok(None))));

        release.notify_waiters();

        harness
            .events
            .wait_for("provider stream start", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamStart {
                            session_id,
                            ..
                        } if session_id == &session_a
                    )
                })
            })
            .await;

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn cancel_stream_persists_partial_message_as_complete() -> Result<()> {
        let gate = Arc::new(tokio::sync::Notify::new());
        let harness = RuntimeTestHarness::new(vec![vec![
            ScriptedChunk::plain("partial "),
            ScriptedChunk::gated("more text", gate),
        ]])
        .await;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("start session"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("first chunk before cancellation", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamChunk {
                            session_id: event_session,
                            chunk,
                            ..
                        } if event_session == &session_id && chunk == "partial "
                    )
                })
            })
            .await;

        assert!(harness.handle.cancel_stream(session_id.clone()).await?);

        let events = harness
            .events
            .wait_for("stream cancelled event", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamCancelled {
                            session_id: event_session,
                            ..
                        } if event_session == &session_id
                    )
                })
            })
            .await;

        assert!(!events.iter().any(|event| {
            matches!(
                event,
                Event::StreamComplete {
                    session_id: event_session,
                    ..
                } if event_session == &session_id
            )
        }));

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        let cancelled_message = history
            .values()
            .find(|message| message.role == ChatRole::Assistant)
            .expect("assistant message should persist");
        assert_eq!(cancelled_message.content, "partial ");
        assert_eq!(cancelled_message.status, MessageStatus::Complete);

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn cancel_stream_before_first_chunk_discards_empty_placeholder() -> Result<()> {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut providers = ProviderManager::new();
        providers.register_provider(
            ProviderId::new("mock"),
            Box::new(BlockingStartProvider {
                id: ProviderId::new("mock"),
                started: started.clone(),
                release,
            }),
        );

        let harness = RuntimeTestHarness::new_with_parts(providers, ToolManager::new()).await;
        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("start session"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;
        started.notified().await;

        assert!(harness.handle.cancel_stream(session_id.clone()).await?);
        harness
            .events
            .wait_for("stream cancelled before first chunk", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamCancelled {
                            session_id: event_session,
                            ..
                        } if event_session == &session_id
                    )
                })
            })
            .await;

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert_eq!(history.len(), 1);
        assert!(
            history
                .values()
                .all(|message| message.role == ChatRole::User)
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn cancel_stream_prevents_tool_detection() -> Result<()> {
        let gate = Arc::new(tokio::sync::Notify::new());
        let harness = RuntimeTestHarness::new(vec![vec![
            ScriptedChunk::plain("before tool\n"),
            ScriptedChunk::gated(
                "<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>",
                gate,
            ),
        ]])
        .await;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("start session"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("pre-tool chunk before cancellation", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamChunk {
                            session_id: event_session,
                            chunk,
                            ..
                        } if event_session == &session_id && chunk == "before tool\n"
                    )
                })
            })
            .await;

        assert!(harness.handle.cancel_stream(session_id.clone()).await?);
        tokio::time::sleep(Duration::from_millis(100)).await;

        let events = harness.events.snapshot();
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                Event::ToolCallDetected {
                    session_id: event_session,
                    ..
                } if event_session == &session_id
            )
        }));

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn cancel_stream_frees_session_for_next_send() -> Result<()> {
        let gate = Arc::new(tokio::sync::Notify::new());
        let harness = RuntimeTestHarness::new(vec![
            vec![
                ScriptedChunk::plain("partial "),
                ScriptedChunk::gated("blocked", gate),
            ],
            vec![ScriptedChunk::plain("second reply")],
        ])
        .await;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("first"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("first stream chunk", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamChunk {
                            session_id: event_session,
                            chunk,
                            ..
                        } if event_session == &session_id && chunk == "partial "
                    )
                })
            })
            .await;

        assert!(harness.handle.cancel_stream(session_id.clone()).await?);
        harness
            .events
            .wait_for("first cancellation", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamCancelled {
                            session_id: event_session,
                            ..
                        } if event_session == &session_id
                    )
                })
            })
            .await;

        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("second"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("second stream completion", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamComplete {
                            session_id: event_session,
                            ..
                        } if event_session == &session_id
                    )
                })
            })
            .await;

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(
            history
                .values()
                .any(|message| message.content == "partial ")
        );
        assert!(
            history
                .values()
                .any(|message| message.content == "second reply")
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn list_sessions_marks_streaming_session_while_active_and_clears_after_cancel()
    -> Result<()> {
        let gate = Arc::new(tokio::sync::Notify::new());
        let harness = RuntimeTestHarness::new(vec![vec![
            ScriptedChunk::plain("partial "),
            ScriptedChunk::gated("blocked", gate),
        ]])
        .await;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("first"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("stream start", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamStart {
                            session_id: event_session,
                            ..
                        } if event_session == &session_id
                    )
                })
            })
            .await;

        let sessions = harness.handle.list_sessions().await?;
        assert!(
            sessions
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| session.is_streaming)
        );

        assert!(harness.handle.cancel_stream(session_id.clone()).await?);
        harness
            .events
            .wait_for("stream cancelled", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamCancelled {
                            session_id: event_session,
                            ..
                        } if event_session == &session_id
                    )
                })
            })
            .await;

        let sessions = harness.handle.list_sessions().await?;
        assert!(
            sessions
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| !session.is_streaming)
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn repeated_malformed_tool_calls_continue_without_deadlocking() -> Result<()> {
        let harness = RuntimeTestHarness::new(vec![
            vec![ScriptedChunk::plain(
                "<tool_call>\n\
tool: edit_file\n\
path: Cargo.toml\n\
create: false\n\
edits: [{\"old_text\":\"rust = \\\"1.88.0\\\"\",\"new_text\":\"rust = \\\"1.90.0\\\"\"}]\n\
</tool_call>",
            )],
            vec![ScriptedChunk::plain("first continuation complete")],
            vec![ScriptedChunk::plain(
                "<tool_call>\n\
tool: edit_file\n\
path: Cargo.toml\n\
create: false\n\
edits: [{\"old_text\":\"rust = \\\"1.90.0\\\"\",\"new_text\":\"rust = \\\"1.91.0\\\"\"}]\n\
</tool_call>",
            )],
            vec![ScriptedChunk::plain("second continuation complete")],
        ])
        .await;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;

        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("first message"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("first malformed tool call continuation", |events| {
                let stream_completions = events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::StreamComplete { session_id: event_session, .. }
                                if event_session == &session_id
                        )
                    })
                    .count();
                stream_completions >= 2
            })
            .await;

        let history_after_first = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(history_after_first.values().any(|message| {
            message.content.contains("Failed to parse tool call")
                && message
                    .content
                    .contains("Expected array length, found LeftBrace")
        }));
        assert!(!harness.events.snapshot().iter().any(|event| {
            matches!(
                event,
                Event::ToolCallDetected {
                    session_id: event_session,
                    ..
                } if event_session == &session_id
            )
        }));
        assert!(
            history_after_first
                .values()
                .any(|message| message.content == "first continuation complete")
        );

        let second_session = create_session_with_profile(&harness.handle, "test-profile").await?;
        let load_result = tokio::time::timeout(
            Duration::from_millis(200),
            harness.handle.load_session(second_session.clone()),
        )
        .await;
        assert!(matches!(load_result, Ok(Ok(true))));

        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("second message"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("second malformed tool call continuation", |events| {
                let stream_completions = events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::StreamComplete { session_id: event_session, .. }
                                if event_session == &session_id
                        )
                    })
                    .count();
                stream_completions >= 4
            })
            .await;

        let history_after_second = harness.handle.get_chat_history(session_id.clone()).await?;
        let parse_failure_count = history_after_second
            .values()
            .filter(|message| message.content.contains("Failed to parse tool call"))
            .count();
        assert_eq!(parse_failure_count, 2);
        assert!(
            history_after_second
                .values()
                .any(|message| message.content == "second continuation complete")
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn thinking_wrapped_tool_call_does_not_emit_tool_detection() -> Result<()> {
        let harness = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain(
            "<think>\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
</think>",
        )]])
        .await;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("hidden tool only"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("thinking-only response completion", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamComplete {
                            session_id: event_session,
                            ..
                        } if event_session == &session_id
                    )
                })
            })
            .await;

        assert!(!harness.events.snapshot().iter().any(|event| {
            matches!(
                event,
                Event::ToolCallDetected {
                    session_id: event_session,
                    ..
                } if event_session == &session_id
            )
        }));

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn only_visible_tool_call_is_detected_when_thinking_wraps_another() -> Result<()> {
        let harness = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain(
            "visible first\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
<thinking class=\"reasoning\">\n\
<tool_call>\n\
tool: mock_tool\n\
value: hidden\n\
</tool_call>\n\
</thinking>",
        )]])
        .await;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("mixed visible and hidden tools"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("single visible tool detection", |events| {
                events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::ToolCallDetected {
                                session_id: event_session,
                                tool_id,
                                ..
                            } if event_session == &session_id && tool_id == "mock_tool"
                        )
                    })
                    .count()
                    == 1
            })
            .await;

        let detections = harness
            .events
            .snapshot()
            .into_iter()
            .filter(|event| {
                matches!(
                    event,
                    Event::ToolCallDetected {
                        session_id: event_session,
                        tool_id,
                        ..
                    } if event_session == &session_id && tool_id == "mock_tool"
                )
            })
            .count();
        assert_eq!(detections, 1);

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn malformed_thinking_block_adds_history_error_and_continues() -> Result<()> {
        let harness = RuntimeTestHarness::new(vec![
            vec![ScriptedChunk::plain(
                "<think>\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>",
            )],
            vec![ScriptedChunk::plain("continuation complete")],
        ])
        .await;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("malformed thinking block"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("thinking parse failure continuation", |events| {
                let stream_completions = events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::StreamComplete {
                                session_id: event_session,
                                ..
                            } if event_session == &session_id
                        )
                    })
                    .count();
                stream_completions >= 2
            })
            .await;

        assert!(!harness.events.snapshot().iter().any(|event| {
            matches!(
                event,
                Event::ToolCallDetected {
                    session_id: event_session,
                    ..
                } if event_session == &session_id
            )
        }));

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(history.values().any(|message| {
            message.content.contains("Failed to parse thinking block")
                && message
                    .content
                    .contains("Missing closing </think> or </thinking> tag")
        }));
        assert!(
            history
                .values()
                .any(|message| message.content == "continuation complete")
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn invalid_tool_arguments_do_not_emit_permission_events() -> Result<()> {
        let harness = RuntimeTestHarness::new_with_tools(
            vec![
                vec![ScriptedChunk::plain(
                    r#"<tool_call>
tool: edit_file
path: /tmp/providers.toml
create: false
edits[1]{old_text,new_text}:
  old,true
</tool_call>"#,
                )],
                vec![ScriptedChunk::plain("continuation complete")],
            ],
            |tools| {
                tools.register_tool(EditFileTool);
            },
        )
        .await;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("trigger invalid edit"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("invalid tool call continuation", |events| {
                let stream_completions = events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::StreamComplete { session_id: event_session, .. }
                                if event_session == &session_id
                        )
                    })
                    .count();
                stream_completions >= 2
            })
            .await;

        assert!(!harness.events.snapshot().iter().any(|event| {
            matches!(
                event,
                Event::ToolCallDetected {
                    session_id: event_session,
                    ..
                } if event_session == &session_id
            )
        }));

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(history.values().any(|message| {
            message
                .content
                .contains("Unable to validate edit_file arguments")
        }));
        assert!(
            history
                .values()
                .any(|message| message.content == "continuation complete")
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn native_toon_edit_file_call_executes_automatically_in_workspace() -> Result<()> {
        let harness = RuntimeTestHarness::new_with_tools(
            vec![
                vec![ScriptedChunk::plain(
                    r#"<tool_call>
tool: edit_file
path: src/lib.rs
create: false
edits[1]{old_text,new_text}:
  old,new
</tool_call>"#,
                )],
                vec![ScriptedChunk::plain("continuation complete")],
            ],
            |tools| {
                tools.register_tool(EditFileTool);
            },
        )
        .await;

        let workspace_src = harness.data_dir.join("workspace").join("src");
        tokio::fs::create_dir_all(&workspace_src).await?;
        tokio::fs::write(workspace_src.join("lib.rs"), "old").await?;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("trigger edit"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("native edit_file execution", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            tool_id,
                            success,
                            denied,
                            ..
                        } if event_session == &session_id
                            && tool_id == "edit_file"
                            && *success
                            && !denied
                    )
                })
            })
            .await;

        let events = harness.events.snapshot();
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                Event::ToolCallDetected {
                    session_id: event_session,
                    tool_id,
                    ..
                } if event_session == &session_id && tool_id == "edit_file"
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                Event::ToolResultReady {
                    session_id: event_session,
                    tool_id,
                    success,
                    denied,
                    ..
                } if event_session == &session_id
                    && tool_id == "edit_file"
                    && *success
                    && !denied
            )
        }));

        harness
            .events
            .wait_for("edit_file continuation", |events| {
                let tool_result_ready = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            tool_id,
                            success,
                            denied,
                            ..
                        } if event_session == &session_id
                            && tool_id == "edit_file"
                            && *success
                            && !denied
                    )
                });
                let stream_completions = events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::StreamComplete {
                                session_id: event_session,
                                ..
                            } if event_session == &session_id
                        )
                    })
                    .count();
                tool_result_ready && stream_completions >= 2
            })
            .await;

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(
            history
                .values()
                .any(|message| message.content == "continuation complete")
        );
        assert_eq!(
            tokio::fs::read_to_string(workspace_src.join("lib.rs")).await?,
            "new"
        );

        harness.shutdown().await;
        Ok(())
    }
}
