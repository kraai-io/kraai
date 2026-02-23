#![deny(clippy::all)]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use agent::AgentManager;
use color_eyre::eyre::{Context, Result, eyre};
use persistence::SessionMeta;
use provider_core::{ProviderManager, ProviderManagerConfig, ProviderManagerHelper};
use tool_core::ToolManager;
use tool_read_file::ReadFileTool;
use types::{MessageId, ModelId, ProviderId};

use futures::StreamExt;
use notify::{RecursiveMode, Watcher};
use provider_google::GoogleFactory;
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
    pub created_at: u64,
    pub updated_at: u64,
    pub title: Option<String>,
}

impl From<SessionMeta> for Session {
    fn from(meta: SessionMeta) -> Self {
        Session {
            id: meta.id,
            tip_id: meta.tip_id.map(|id| id.to_string()),
            created_at: meta.created_at,
            updated_at: meta.updated_at,
            title: meta.title,
        }
    }
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

                let agent_manager = Arc::new(Mutex::new(AgentManager::new(
                    providers,
                    tools,
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

            // Emit tool call events
            for (call_id, tool_id, description) in tool_calls {
                let args_json = {
                    let agent = agent_manager.lock().await;
                    agent
                        .get_pending_tool_args(&call_id)
                        .map(|a| serde_json::to_string(&a).unwrap_or_default())
                        .unwrap_or_default()
                };

                tracing::debug!(
                    "Emitting ToolCallDetected: {} - {}",
                    tool_id,
                    description
                );
                event_callback.on_event(Event::ToolCallDetected {
                    call_id: call_id.to_string(),
                    tool_id,
                    args: args_json,
                    description,
                });
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

            // Start continuation stream
            Self::start_continuation(agent_manager, event_callback, command_tx).await;
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

            for (call_id, tool_id, description) in tool_calls {
                let args_json = {
                    let agent = agent_manager.lock().await;
                    agent
                        .get_pending_tool_args(&call_id)
                        .map(|a| serde_json::to_string(&a).unwrap_or_default())
                        .unwrap_or_default()
                };

                event_callback.on_event(Event::ToolCallDetected {
                    call_id: call_id.to_string(),
                    tool_id,
                    args: args_json,
                    description,
                });
            }

            let _ = command_tx;
        })
    }

    fn spawn_config_watcher(&self) {
        let command_tx = self.command_tx.clone();
        let callback = self.event_callback.clone();

        tokio::spawn(async move {
            let config_loc = directories::BaseDirs::new()
                .expect("Failed to find user directories")
                .home_dir()
                .join(".agent-desktop/providers.toml");

            let (tx, rx) = std::sync::mpsc::channel();
            let mut watcher =
                notify::recommended_watcher(tx).expect("Failed to create config watcher");

            watcher
                .watch(&config_loc, RecursiveMode::NonRecursive)
                .expect("Failed to watch config file");

            for res in rx {
                match res {
                    Ok(event) => {
                        if event.kind.is_access() {
                            continue;
                        }
                        if event.kind.is_remove() {
                            let _ = watcher.watch(&config_loc, RecursiveMode::NonRecursive);
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
        let config_loc = directories::BaseDirs::new()
            .expect("Failed to find user directories")
            .home_dir()
            .join(".agent-desktop/providers.toml");

        if !config_loc.exists() {
            return Err(eyre!("Config file doesn't exist at {:?}", config_loc));
        }

        let config_slice = tokio::fs::read(&config_loc).await?;
        let config: ProviderManagerConfig =
            toml::from_slice(&config_slice).wrap_err("Failed to parse providers.toml")?;

        let mut helper = ProviderManagerHelper::default();
        helper
            .register_factory::<GoogleFactory>()
            .map_err(|e| eyre!("{}", e))?;
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
}
