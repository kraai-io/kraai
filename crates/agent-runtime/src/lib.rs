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
use tool_read_file::ReadFileTool;
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
    StreamStart { message_id: String },
    /// Chunk received for a streaming message
    StreamChunk { message_id: String, chunk: String },
    /// Stream completed for a message
    StreamComplete { message_id: String },
    /// Stream error for a message
    StreamError { message_id: String, error: String },

    // Tool events
    /// Tool call detected, awaiting permission
    ToolCallDetected {
        call_id: String,
        tool_id: String,
        args: String,
        description: String,
        risk_level: String,
        reasons: Vec<String>,
    },
    /// Tool execution result ready
    ToolResultReady {
        call_id: String,
        tool_id: String,
        success: bool,
        output: String,
        denied: bool,
    },

    // History events
    /// Chat history was updated
    HistoryUpdated,
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
    SendMessage {
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
    },
    LoadConfig,
    ClearCurrentSession,
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
    GetCurrentSessionId {
        response: oneshot::Sender<Option<String>>,
    },
    GetCurrentWorkspaceState {
        response: oneshot::Sender<Option<WorkspaceState>>,
    },
    SetCurrentWorkspaceDir {
        workspace_dir: String,
        response: oneshot::Sender<()>,
    },
    GetCurrentTip {
        response: oneshot::Sender<Option<String>>,
    },
    GetChatHistory {
        response: oneshot::Sender<BTreeMap<MessageId, types::Message>>,
    },
    ApproveTool {
        call_id: String,
    },
    DenyTool {
        call_id: String,
    },
    ExecuteApprovedTools,
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

    /// Send a message to the agent
    pub async fn send_message(
        &self,
        message: String,
        model_id: String,
        provider_id: String,
    ) -> Result<()> {
        self.command_tx
            .send(Command::SendMessage {
                message,
                model_id: ModelId::new(model_id),
                provider_id: ProviderId::new(provider_id),
            })
            .await?;
        Ok(())
    }

    /// Get the chat history as a tree
    pub async fn get_chat_history(&self) -> Result<BTreeMap<MessageId, types::Message>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetChatHistory { response: tx })
            .await?;
        Ok(rx.await?)
    }

    /// Clear the current session
    pub async fn clear_current_session(&self) -> Result<()> {
        self.command_tx.send(Command::ClearCurrentSession).await?;
        Ok(())
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

    /// Get the current session ID
    pub async fn get_current_session_id(&self) -> Result<Option<String>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetCurrentSessionId { response: tx })
            .await?;
        Ok(rx.await?)
    }

    pub async fn get_current_workspace_state(&self) -> Result<Option<WorkspaceState>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetCurrentWorkspaceState { response: tx })
            .await?;
        Ok(rx.await?)
    }

    pub async fn set_current_workspace_dir(&self, workspace_dir: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SetCurrentWorkspaceDir {
                workspace_dir,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    /// Get the current tip message ID
    pub async fn get_current_tip(&self) -> Result<Option<String>> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetCurrentTip { response: tx })
            .await?;
        Ok(rx.await?)
    }

    /// Approve a tool call
    pub async fn approve_tool(&self, call_id: String) -> Result<()> {
        self.command_tx
            .send(Command::ApproveTool { call_id })
            .await?;
        Ok(())
    }

    /// Deny a tool call
    pub async fn deny_tool(&self, call_id: String) -> Result<()> {
        self.command_tx.send(Command::DenyTool { call_id }).await?;
        Ok(())
    }

    /// Execute all approved tools
    pub async fn execute_approved_tools(&self) -> Result<()> {
        self.command_tx.send(Command::ExecuteApprovedTools).await?;
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
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                Self::init_tracing();

                let (message_store, session_store) = persistence::init()
                    .await
                    .expect("Failed to initialize persistence layer");

                let providers = ProviderManager::new();
                let mut tools = ToolManager::new();
                tools.register_tool(ReadFileTool {});
                let default_workspace_dir = std::env::current_dir()
                    .and_then(|path| path.canonicalize())
                    .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());

                let agent_manager = Arc::new(Mutex::new(AgentManager::new(
                    providers,
                    tools,
                    default_workspace_dir,
                    message_store,
                    session_store,
                )));

                let runtime = RuntimeInner {
                    event_callback: callback,
                    command_tx: command_tx_for_runtime,
                    agent_manager,
                };

                runtime.run(command_rx).await;
            });
        });

        handle
    }

    fn init_tracing() {
        use std::sync::Once;
        static INIT: Once = Once::new();

        INIT.call_once(|| {
            let log_dir = directories::BaseDirs::new()
                .expect("Failed to find user directories")
                .home_dir()
                .join(".agent-desktop/logs");

            std::fs::create_dir_all(&log_dir).expect("Failed to create log directory");

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
                .expect("Failed to set tracing subscriber");
        });
    }
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

            Command::LoadConfig => {
                self.load_providers_config().await?;
                tracing::info!("Loaded config");
                self.send_event(Event::ConfigLoaded);
            }

            Command::SendMessage {
                message,
                model_id,
                provider_id,
            } => {
                self.handle_send_message(message, model_id, provider_id)
                    .await;
            }

            Command::ClearCurrentSession => {
                self.agent_manager
                    .lock()
                    .await
                    .clear_current_session()
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
                    .load_session(&session_id)
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

            Command::GetCurrentSessionId { response } => {
                let session_id = self
                    .agent_manager
                    .lock()
                    .await
                    .get_current_session_id()
                    .map(|s| s.to_string());
                response
                    .send(session_id)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::GetCurrentWorkspaceState { response } => {
                let workspace_state = self
                    .agent_manager
                    .lock()
                    .await
                    .get_current_workspace_dir_state()
                    .map(|(workspace_dir, applies_next_chat)| WorkspaceState {
                        workspace_dir: workspace_dir.display().to_string(),
                        applies_next_chat,
                    });
                response
                    .send(workspace_state)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::SetCurrentWorkspaceDir {
                workspace_dir,
                response,
            } => {
                let workspace_dir = canonicalize_workspace_dir(&workspace_dir)?;
                self.agent_manager
                    .lock()
                    .await
                    .set_current_workspace_dir(workspace_dir)
                    .await?;
                response.send(()).map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::GetCurrentTip { response } => {
                let tip_id = self
                    .agent_manager
                    .lock()
                    .await
                    .get_current_tip()
                    .await?
                    .map(|id| id.to_string());
                response
                    .send(tip_id)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::GetChatHistory { response } => {
                let history = self.agent_manager.lock().await.get_chat_history().await?;
                response
                    .send(history)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }

            Command::ApproveTool { call_id } => {
                let call_id = types::CallId::new(call_id);
                self.agent_manager.lock().await.approve_tool(call_id);
            }

            Command::DenyTool { call_id } => {
                let call_id = types::CallId::new(call_id);
                self.agent_manager.lock().await.deny_tool(call_id);
            }

            Command::ExecuteApprovedTools => {
                self.handle_execute_tools().await;
            }
        }

        Ok(())
    }

    async fn handle_send_message(
        &self,
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
                match agent.start_stream(message, model_id, provider_id).await {
                    Ok(result) => result,
                    Err(e) => {
                        event_callback.on_event(Event::Error(e.to_string()));
                        return;
                    }
                }
            };

            // Send stream start event
            event_callback.on_event(Event::StreamStart {
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
                            agent.append_chunk(&msg_id, &chunk).await;
                        }

                        // Send chunk event
                        event_callback.on_event(Event::StreamChunk {
                            message_id: msg_id.to_string(),
                            chunk,
                        });
                    }
                    Err(e) => {
                        event_callback.on_event(Event::StreamError {
                            message_id: msg_id.to_string(),
                            error: e.to_string(),
                        });
                        return;
                    }
                }
            }

            tracing::debug!("Full content length: {}", full_content.len());

            // Complete the message
            {
                let agent = agent_manager.lock().await;
                let _ = agent.complete_message(&msg_id).await;
            }

            // Send completion event
            event_callback.on_event(Event::StreamComplete {
                message_id: msg_id.to_string(),
            });

            // Notify UI that history was updated
            event_callback.on_event(Event::HistoryUpdated);

            // Check for tool calls
            let (tool_calls, failed) = {
                let mut agent = agent_manager.lock().await;
                agent.parse_tool_calls_from_content(&full_content).await
            };

            tracing::debug!(
                "Found {} tool calls, {} failed",
                tool_calls.len(),
                failed.len()
            );

            if !failed.is_empty() {
                tracing::warn!("Failed tool calls found, adding to history");
                let mut agent = agent_manager.lock().await;
                let _ = agent.add_failed_tool_calls_to_history(failed).await;
                event_callback.on_event(Event::HistoryUpdated);

                // Start continuation stream
                Self::start_continuation(agent_manager.clone(), event_callback.clone(), command_tx)
                    .await;
                return;
            }

            let mut has_auto_approved_tools = false;

            // Emit tool call events
            for tool_call in tool_calls {
                let args_json = {
                    let agent = agent_manager.lock().await;
                    agent
                        .get_pending_tool_args(&tool_call.call_id)
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
                let _ = command_tx.send(Command::ExecuteApprovedTools).await;
            }
        });
    }

    async fn handle_execute_tools(&self) {
        let event_callback = self.event_callback.clone();
        let agent_manager = self.agent_manager.clone();
        let command_tx = self.command_tx.clone();

        tokio::spawn(async move {
            let results = {
                let mut agent = agent_manager.lock().await;
                agent.execute_approved_tools().await
            };

            for result in &results {
                let success = !result.output.get("error").is_some_and(|e| e.is_string());
                let output = serde_json::to_string(&result.output).unwrap_or_default();

                event_callback.on_event(Event::ToolResultReady {
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
                let _ = agent.add_tool_results_to_history(results).await;
            }

            tracing::debug!("Emitting HistoryUpdated event after tool results");
            event_callback.on_event(Event::HistoryUpdated);

            let has_pending_tools = { agent_manager.lock().await.has_pending_tools() };
            if !has_pending_tools {
                // Start continuation stream
                Self::start_continuation(agent_manager, event_callback, command_tx).await;
            }
        });
    }

    fn start_continuation(
        agent_manager: Arc<Mutex<AgentManager>>,
        event_callback: Arc<dyn EventCallback>,
        command_tx: mpsc::Sender<Command>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            let continuation = {
                let mut agent = agent_manager.lock().await;
                agent.start_continuation_stream().await
            };

            let Ok(Some((msg_id, mut stream))) = continuation else {
                return;
            };

            event_callback.on_event(Event::StreamStart {
                message_id: msg_id.to_string(),
            });

            let mut content = String::new();

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        content.push_str(&chunk);
                        {
                            let agent = agent_manager.lock().await;
                            agent.append_chunk(&msg_id, &chunk).await;
                        }
                        event_callback.on_event(Event::StreamChunk {
                            message_id: msg_id.to_string(),
                            chunk,
                        });
                    }
                    Err(e) => {
                        tracing::error!("Continuation stream error: {}", e);
                        event_callback.on_event(Event::StreamError {
                            message_id: msg_id.to_string(),
                            error: e.to_string(),
                        });
                        return;
                    }
                }
            }

            {
                let agent = agent_manager.lock().await;
                let _ = agent.complete_message(&msg_id).await;
            }

            event_callback.on_event(Event::StreamComplete {
                message_id: msg_id.to_string(),
            });

            // Check for more tool calls
            let (tool_calls, failed) = {
                let mut agent = agent_manager.lock().await;
                agent.parse_tool_calls_from_content(&content).await
            };

            if !failed.is_empty() {
                {
                    let mut agent = agent_manager.lock().await;
                    let _ = agent.add_failed_tool_calls_to_history(failed).await;
                }
                event_callback.on_event(Event::HistoryUpdated);

                // Recurse for retry
                Self::start_continuation(agent_manager, event_callback, command_tx).await;
                return;
            }

            let mut has_auto_approved_tools = false;

            for tool_call in tool_calls {
                let args_json = {
                    let agent = agent_manager.lock().await;
                    agent
                        .get_pending_tool_args(&tool_call.call_id)
                        .map(|a| serde_json::to_string(&a).unwrap_or_default())
                        .unwrap_or_default()
                };

                if tool_call.requires_confirmation {
                    event_callback.on_event(Event::ToolCallDetected {
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
                let _ = command_tx.send(Command::ExecuteApprovedTools).await;
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
            let mut watcher =
                notify::recommended_watcher(tx).expect("Failed to create config watcher");

            watcher
                .watch(&config_dir, RecursiveMode::NonRecursive)
                .expect("Failed to watch config directory");

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
        return Err(eyre!("Workspace directory does not exist: {}", raw.display()));
    }
    if !raw.is_dir() {
        return Err(eyre!("Workspace path is not a directory: {}", raw.display()));
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
    let table = as_table(&config.config);
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
    let table = as_table(&config.config);
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

fn as_table(value: &toml::Value) -> &toml::map::Map<String, toml::Value> {
    value
        .as_table()
        .expect("provider/model config should be a table")
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
