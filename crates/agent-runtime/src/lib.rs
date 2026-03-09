#![deny(clippy::all)]

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use agent::AgentManager;
use color_eyre::eyre::{Context, ContextCompat, Result, eyre};
use persistence::SessionMeta;
use provider_core::{
    ModelConfig, ProviderConfig, ProviderManager, ProviderManagerConfig, ProviderManagerHelper,
};
use tool_core::ToolManager;
use tool_edit_file::EditFileTool;
use tool_list_files::ListFilesTool;
use tool_read_file::ReadFileTool;
use tool_search_files::SearchFilesTool;
use types::{MessageId, ModelId, ProviderId};

use futures::StreamExt;
use notify::{RecursiveMode, Watcher};
use provider_openai::OpenAIFactory;
use tokio::sync::{Mutex, mpsc, oneshot};

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
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorkspaceState {
    pub workspace_dir: String,
    pub applies_next_chat: bool,
}

/// Supported provider types for settings editing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderType {
    OpenAi,
}

/// Editable provider settings shared across clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSettings {
    pub id: String,
    pub provider_type: ProviderType,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub env_var_api_key: Option<String>,
    pub only_listed_models: bool,
}

/// Editable model settings shared across clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSettings {
    pub id: String,
    pub provider_id: String,
    pub name: Option<String>,
    pub max_context: Option<u32>,
}

/// Full editable settings document persisted to providers.toml.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsDocument {
    pub providers: Vec<ProviderSettings>,
    pub models: Vec<ModelSettings>,
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

    // History events
    /// Chat history was updated
    HistoryUpdated { session_id: String },
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
    GetSettings {
        response: oneshot::Sender<SettingsDocument>,
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
    ApproveTool {
        session_id: String,
        call_id: String,
    },
    DenyTool {
        session_id: String,
        call_id: String,
    },
    ExecuteApprovedTools {
        session_id: String,
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

    /// Get the editable settings document.
    pub async fn get_settings(&self) -> Result<SettingsDocument> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetSettings { response: tx })
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

    /// Execute all approved tools
    pub async fn execute_approved_tools(&self, session_id: String) -> Result<()> {
        self.command_tx
            .send(Command::ExecuteApprovedTools { session_id })
            .await?;
        Ok(())
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
                let log_dir = directories::BaseDirs::new()
                    .context("Failed to find user directories")?
                    .home_dir()
                    .join(".agent-desktop/logs");

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
    tools.register_tool(ReadFileTool);
    tools.register_tool(ListFilesTool);
    tools.register_tool(SearchFilesTool);
    tools.register_tool(EditFileTool);
    tools
}

// ============================================================================
// Runtime Inner - the actual runtime implementation
// ============================================================================

struct RuntimeInner {
    event_callback: Arc<dyn EventCallback>,
    command_tx: mpsc::Sender<Command>,
    agent_manager: Arc<Mutex<AgentManager>>,
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

            Command::GetSettings { response } => {
                let settings = read_settings_document(&settings_path()?)?;
                response
                    .send(settings)
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
                let sessions = self.agent_manager.lock().await.list_sessions().await?;
                let sessions: Vec<Session> = sessions.into_iter().map(Into::into).collect();
                response
                    .send(sessions)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::DeleteSession { session_id } => {
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

            Command::ExecuteApprovedTools { session_id } => {
                self.handle_execute_tools(session_id).await;
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
        let event_callback = self.event_callback.clone();
        let agent_manager = self.agent_manager.clone();
        let command_tx = self.command_tx.clone();

        tokio::spawn(async move {
            let (msg_id, mut stream) = {
                let mut agent = agent_manager.lock().await;
                match agent
                    .start_stream(&session_id, message, model_id, provider_id)
                    .await
                {
                    Ok(result) => result,
                    Err(e) => {
                        event_callback.on_event(Event::Error(e.to_string()));
                        return;
                    }
                }
            };

            // Send stream start event
            event_callback.on_event(Event::StreamStart {
                session_id: session_id.clone(),
                message_id: msg_id.to_string(),
            });

            let mut full_content = String::new();

            // Process stream chunks
            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        full_content.push_str(&chunk);

                        // Append to message in session
                        {
                            let agent = agent_manager.lock().await;
                            if !agent.append_chunk(&msg_id, &chunk).await {
                                return;
                            }
                        }

                        // Send chunk event
                        event_callback.on_event(Event::StreamChunk {
                            session_id: session_id.clone(),
                            message_id: msg_id.to_string(),
                            chunk,
                        });
                    }
                    Err(e) => {
                        {
                            let agent = agent_manager.lock().await;
                            let _ = agent.abort_streaming_message(&msg_id).await;
                        }
                        event_callback.on_event(Event::StreamError {
                            session_id: session_id.clone(),
                            message_id: msg_id.to_string(),
                            error: e.to_string(),
                        });
                        return;
                    }
                }
            }

            tracing::debug!("Full content length: {}", full_content.len());

            // Complete the message
            let completed_session = {
                let agent = agent_manager.lock().await;
                agent.complete_message(&msg_id).await
            };
            let Ok(Some(completed_session)) = completed_session else {
                return;
            };

            // Send completion event
            event_callback.on_event(Event::StreamComplete {
                session_id: completed_session.clone(),
                message_id: msg_id.to_string(),
            });

            // Notify UI that history was updated
            event_callback.on_event(Event::HistoryUpdated {
                session_id: completed_session.clone(),
            });

            // Check for tool calls
            let (tool_calls, failed) = {
                let mut agent = agent_manager.lock().await;
                match agent
                    .parse_tool_calls_from_content(&completed_session, &full_content)
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
                let mut agent = agent_manager.lock().await;
                let _ = agent
                    .add_failed_tool_calls_to_history(&completed_session, failed)
                    .await;
                event_callback.on_event(Event::HistoryUpdated {
                    session_id: completed_session.clone(),
                });

                // Start continuation stream
                Self::start_continuation(
                    completed_session,
                    agent_manager.clone(),
                    event_callback.clone(),
                    command_tx,
                )
                .await;
                return;
            }

            let mut has_auto_approved_tools = false;

            // Emit tool call events
            for tool_call in tool_calls {
                let args_json = {
                    let agent = agent_manager.lock().await;
                    agent
                        .get_pending_tool_args(&completed_session, &tool_call.call_id)
                        .map(|a| serde_json::to_string(&a).unwrap_or_default())
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
            }
        });
    }

    async fn handle_execute_tools(&self, session_id: String) {
        let event_callback = self.event_callback.clone();
        let agent_manager = self.agent_manager.clone();
        let command_tx = self.command_tx.clone();

        tokio::spawn(async move {
            let results = {
                let mut agent = agent_manager.lock().await;
                agent.execute_approved_tools(&session_id).await
            };

            for result in &results {
                let success = !result.output.get("error").is_some_and(|e| e.is_string());
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
                Self::start_continuation(session_id, agent_manager, event_callback, command_tx)
                    .await;
            }
        });
    }

    fn start_continuation(
        session_id: String,
        agent_manager: Arc<Mutex<AgentManager>>,
        event_callback: Arc<dyn EventCallback>,
        command_tx: mpsc::Sender<Command>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            let continuation = {
                let mut agent = agent_manager.lock().await;
                agent.start_continuation_stream(&session_id).await
            };

            let Ok(Some((msg_id, mut stream))) = continuation else {
                return;
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
                                return;
                            }
                        }
                        event_callback.on_event(Event::StreamChunk {
                            session_id: session_id.clone(),
                            message_id: msg_id.to_string(),
                            chunk,
                        });
                    }
                    Err(e) => {
                        tracing::error!("Continuation stream error: {}", e);
                        {
                            let agent = agent_manager.lock().await;
                            let _ = agent.abort_streaming_message(&msg_id).await;
                        }
                        event_callback.on_event(Event::StreamError {
                            session_id: session_id.clone(),
                            message_id: msg_id.to_string(),
                            error: e.to_string(),
                        });
                        return;
                    }
                }
            }

            let completed_session = {
                let agent = agent_manager.lock().await;
                agent.complete_message(&msg_id).await
            };
            let Ok(Some(completed_session)) = completed_session else {
                return;
            };

            event_callback.on_event(Event::StreamComplete {
                session_id: completed_session.clone(),
                message_id: msg_id.to_string(),
            });
            event_callback.on_event(Event::HistoryUpdated {
                session_id: completed_session.clone(),
            });

            // Check for more tool calls
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

            if !failed.is_empty() {
                {
                    let mut agent = agent_manager.lock().await;
                    let _ = agent
                        .add_failed_tool_calls_to_history(&completed_session, failed)
                        .await;
                }
                event_callback.on_event(Event::HistoryUpdated {
                    session_id: completed_session.clone(),
                });

                // Recurse for retry
                Self::start_continuation(
                    completed_session,
                    agent_manager,
                    event_callback,
                    command_tx,
                )
                .await;
                return;
            }

            let mut has_auto_approved_tools = false;

            for tool_call in tool_calls {
                let args_json = {
                    let agent = agent_manager.lock().await;
                    agent
                        .get_pending_tool_args(&completed_session, &tool_call.call_id)
                        .map(|a| serde_json::to_string(&a).unwrap_or_default())
                        .unwrap_or_default()
                };

                if tool_call.requires_confirmation {
                    event_callback.on_event(Event::ToolCallDetected {
                        session_id: completed_session.clone(),
                        call_id: tool_call.call_id.to_string(),
                        tool_id: tool_call.tool_id,
                        args: args_json,
                        description: tool_call.description,
                        risk_level: tool_call.assessment.risk.as_str().to_string(),
                        reasons: tool_call.assessment.reasons,
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
            }
        })
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

        if !config_loc.exists() {
            return Err(eyre!("Config file doesn't exist at {:?}", config_loc));
        }

        let config_slice = tokio::fs::read(&config_loc).await?;
        let config: ProviderManagerConfig =
            toml::from_slice(&config_slice).wrap_err("Failed to parse providers.toml")?;

        let mut helper = ProviderManagerHelper::default();
        helper
            .register_factory::<OpenAIFactory>()
            .map_err(|e| eyre!("{}", e))?;

        self.agent_manager
            .lock()
            .await
            .set_providers(config, helper)
            .await?;

        Ok(())
    }

    async fn save_settings_document(&self, settings: SettingsDocument) -> Result<()> {
        let config_loc = settings_path()?;
        write_settings_document(&config_loc, &settings).await?;
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
    Ok(directories::BaseDirs::new()
        .context("Failed to find user directories")?
        .home_dir()
        .join(".agent-desktop/providers.toml"))
}

fn read_settings_document(path: &std::path::Path) -> Result<SettingsDocument> {
    if !path.exists() {
        return Ok(SettingsDocument::default());
    }

    let config_slice = std::fs::read(path)?;
    let config: ProviderManagerConfig =
        toml::from_slice(&config_slice).wrap_err("Failed to parse providers.toml")?;
    settings_from_provider_config(config)
}

async fn write_settings_document(
    path: &std::path::Path,
    settings: &SettingsDocument,
) -> Result<()> {
    let errors = validate_settings(settings);
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

fn settings_from_provider_config(config: ProviderManagerConfig) -> Result<SettingsDocument> {
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

    Ok(SettingsDocument { providers, models })
}

fn provider_settings_from_config(config: ProviderConfig) -> Result<ProviderSettings> {
    let provider_type = match config.r#type.as_str() {
        "openai" => ProviderType::OpenAi,
        other => return Err(eyre!("Unsupported provider type in settings: {other}")),
    };
    let table = as_table(&config.config)?;
    let base_url = table_string(table, "base_url");
    let api_key = table_string(table, "api_key");
    let env_var_api_key = table_string(table, "env_var_api_key");
    let only_listed_models = table_bool(table, "only_listed_models").unwrap_or(false);

    Ok(ProviderSettings {
        id: config.id.to_string(),
        provider_type,
        base_url,
        api_key,
        env_var_api_key,
        only_listed_models,
    })
}

fn model_settings_from_config(config: ModelConfig) -> Result<ModelSettings> {
    let table = as_table(&config.config)?;
    let id = table_string(table, "id").ok_or_else(|| eyre!("Model config missing id"))?;
    let name = table_string(table, "name");
    let max_context = table
        .get("max_context")
        .and_then(toml::Value::as_integer)
        .map(|value| u32::try_from(value).map_err(|_| eyre!("Invalid max_context: {value}")))
        .transpose()?;

    Ok(ModelSettings {
        id,
        provider_id: config.provider_id.to_string(),
        name,
        max_context,
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
    let mut table = toml::map::Map::new();
    match settings.provider_type {
        ProviderType::OpenAi => {
            let base_url = trim_optional(&settings.base_url)
                .ok_or_else(|| eyre!("OpenAI providers require a base_url"))?;
            table.insert(String::from("base_url"), toml::Value::String(base_url));
        }
    }

    if let Some(api_key) = trim_optional(&settings.api_key) {
        table.insert(String::from("api_key"), toml::Value::String(api_key));
    }
    if let Some(env_var_api_key) = trim_optional(&settings.env_var_api_key) {
        table.insert(
            String::from("env_var_api_key"),
            toml::Value::String(env_var_api_key),
        );
    }
    table.insert(
        String::from("only_listed_models"),
        toml::Value::Boolean(settings.only_listed_models),
    );

    Ok(ProviderConfig {
        id: ProviderId::new(settings.id.trim().to_string()),
        r#type: match settings.provider_type {
            ProviderType::OpenAi => String::from("openai"),
        },
        config: toml::Value::Table(table),
    })
}

fn model_config_entry_from_settings(settings: &ModelSettings) -> Result<ModelConfig> {
    let mut table = toml::map::Map::new();
    table.insert(
        String::from("id"),
        toml::Value::String(settings.id.trim().to_string()),
    );
    if let Some(name) = trim_optional(&settings.name) {
        table.insert(String::from("name"), toml::Value::String(name));
    }
    if let Some(max_context) = settings.max_context {
        table.insert(
            String::from("max_context"),
            toml::Value::Integer(i64::from(max_context)),
        );
    }

    Ok(ModelConfig {
        provider_id: ProviderId::new(settings.provider_id.trim().to_string()),
        config: toml::Value::Table(table),
    })
}

fn validate_settings(settings: &SettingsDocument) -> Vec<SettingsValidationError> {
    let mut errors = Vec::new();
    let mut provider_ids = std::collections::BTreeSet::new();

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

        if matches!(provider.provider_type, ProviderType::OpenAi)
            && trim_optional(&provider.base_url).is_none()
        {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.base_url"),
                message: String::from("Base URL is required for OpenAI-compatible providers"),
            });
        }

        if trim_optional(&provider.api_key).is_none()
            && trim_optional(&provider.env_var_api_key).is_none()
        {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.credentials"),
                message: String::from("Provide either an API key or an environment variable name"),
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
        } else if !provider_ids.contains(model.provider_id.trim()) {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.provider_id"),
                message: String::from("Model must reference an existing provider"),
            });
        }
        if let Some(max_context) = model.max_context
            && max_context == 0
        {
            errors.push(SettingsValidationError {
                field: format!("{field_prefix}.max_context"),
                message: String::from("Max context must be greater than zero"),
            });
        }
    }

    errors
}

fn as_table(value: &toml::Value) -> Result<&toml::map::Map<String, toml::Value>> {
    value
        .as_table()
        .ok_or_else(|| eyre!("provider/model config should be a table"))
}

fn table_string(table: &toml::map::Map<String, toml::Value>, key: &str) -> Option<String> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .map(ToString::to_string)
}

fn table_bool(table: &toml::map::Map<String, toml::Value>, key: &str) -> Option<bool> {
    table.get(key).and_then(toml::Value::as_bool)
}

fn trim_optional(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
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
    use tool_core::{Tool, ToolContext, ToolManager, ToolOutput};
    use types::{
        ChatMessage, ChatRole, ExecutionPolicy, ModelId, ProviderId, RiskLevel, ToolCallAssessment,
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

    struct ApprovalTool;

    #[async_trait]
    impl Tool for ApprovalTool {
        fn name(&self) -> &'static str {
            "mock_tool"
        }

        fn schema(&self) -> &'static str {
            "mock_tool(value: string)"
        }

        fn assess(&self, args: &serde_json::Value, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
            ToolCallAssessment {
                risk: RiskLevel::UndoableWorkspaceWrite,
                policy: ExecutionPolicy::AlwaysAsk,
                reasons: vec![format!(
                    "mock_tool requires approval for {:?}",
                    args.get("value")
                )],
            }
        }

        async fn call(&self, args: serde_json::Value, _ctx: &ToolContext<'_>) -> ToolOutput {
            ToolOutput::success(serde_json::json!({
                "tool": "mock_tool",
                "value": args.get("value").cloned().unwrap_or(serde_json::Value::Null),
            }))
        }

        async fn describe(&self, args: serde_json::Value) -> String {
            let value = args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            format!("Mock tool for {value}")
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

            let message_store = Arc::new(FileMessageStore::new(&data_dir));
            let session_store = Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));

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

            let runtime = RuntimeInner {
                event_callback: Arc::new(events.clone()),
                command_tx,
                agent_manager,
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

        let session_a = harness.handle.create_session().await?;
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

        let session_b = harness.handle.create_session().await?;
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

        let session_a = harness.handle.create_session().await?;
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

        let session_b = harness.handle.create_session().await?;
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
}
