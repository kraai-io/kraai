use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use color_eyre::eyre::{Context, Result, eyre};
use futures::StreamExt;
use kraai_agent::{AgentManager, PendingStreamRequest, ToolExecutionPayload, ToolExecutionRequest};
use kraai_persistence::agent_state_root;
use kraai_provider_core::{
    ProviderManager, ProviderManagerConfig, ProviderRegistry, ProviderRequestContext,
    ProviderRetryEvent, ProviderRetryObserver,
};
use kraai_provider_openai_chat_completions::{OpenAiChatCompletionsFactory, OpenAiFactory};
use kraai_provider_openai_codex::{
    OpenAiCodexAuthController, OpenAiCodexAuthStatus as ProviderOpenAiCodexAuthStatus,
    OpenAiCodexFactory, OpenAiCodexLoginState as ProviderOpenAiCodexLoginState,
};
use kraai_tool_close_file::CloseFileTool;
use kraai_tool_core::{ToolContext, ToolManager, ToolOutput};
use kraai_tool_edit_file::EditFileTool;
use kraai_tool_list_files::ListFilesTool;
use kraai_tool_open_file::OpenFileTool;
use kraai_tool_read_file::ReadFileTool;
use kraai_tool_search_files::SearchFilesTool;
use kraai_types::{MessageId, ModelId, ProviderId};
use notify::{RecursiveMode, Watcher};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::task::AbortHandle;

use crate::api::{
    Event, Model, OpenAiCodexAuthStatus, OpenAiCodexLoginState, PendingBrowserLogin,
    PendingDeviceCodeLogin, PendingToolInfo, Session, WorkspaceState,
};
use crate::handle::{Command, RuntimeHandle};
use crate::settings::{
    SettingsDocument, read_settings_document, resolve_provider_config_path, write_settings_document,
};

struct RuntimeRetryObserver {
    session_id: String,
    provider_id: ProviderId,
    model_id: ModelId,
    event_tx: broadcast::Sender<Event>,
}

impl ProviderRetryObserver for RuntimeRetryObserver {
    fn on_retry_scheduled(&self, event: &ProviderRetryEvent) {
        emit_event(
            &self.event_tx,
            Event::ProviderRetryScheduled {
                session_id: self.session_id.clone(),
                provider_id: self.provider_id.to_string(),
                model_id: self.model_id.to_string(),
                operation: event.operation.to_string(),
                retry_number: event.retry_number,
                delay_seconds: event.delay.as_secs(),
                reason: event.reason.clone(),
            },
        );
    }
}

pub(crate) fn emit_event(event_tx: &broadcast::Sender<Event>, event: Event) {
    let _ = event_tx.send(event);
}

/// Builder for creating a runtime
pub struct RuntimeBuilder {
    provider_config_path: Option<PathBuf>,
}

impl RuntimeBuilder {
    /// Create a new runtime builder.
    pub fn new() -> Self {
        Self {
            provider_config_path: None,
        }
    }

    pub fn provider_config_path(mut self, path: PathBuf) -> Self {
        self.provider_config_path = Some(path);
        self
    }

    /// Build and start the runtime
    ///
    /// This spawns the runtime in a background thread and returns a handle
    /// to send commands.
    pub fn build(self) -> RuntimeHandle {
        let (command_tx, command_rx) = mpsc::channel(100);
        let (event_tx, _) = broadcast::channel(1024);
        let handle = RuntimeHandle {
            command_tx,
            event_tx: event_tx.clone(),
        };
        let command_tx_for_runtime = handle.command_tx.clone();

        let provider_config_path = self.provider_config_path.clone();

        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(error) => {
                    emit_event(
                        &event_tx,
                        Event::Error(format!("Failed to create tokio runtime: {error}")),
                    );
                    return;
                }
            };

            if let Err(error) = rt.block_on(Self::run_background(
                event_tx.clone(),
                command_tx_for_runtime,
                command_rx,
                provider_config_path,
            )) {
                emit_event(&event_tx, Event::Error(error.to_string()));
            }
        });

        handle
    }

    async fn run_background(
        event_tx: broadcast::Sender<Event>,
        command_tx: mpsc::Sender<Command>,
        command_rx: mpsc::Receiver<Command>,
        provider_config_path_override: Option<PathBuf>,
    ) -> Result<()> {
        Self::init_tracing()?;

        let (message_store, session_store) = kraai_persistence::init()
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
        let provider_config_path = resolve_provider_config_path(provider_config_path_override)?;

        let agent_manager = Arc::new(Mutex::new(AgentManager::new(
            providers,
            tools,
            default_workspace_dir,
            message_store,
            session_store,
        )));

        let runtime = RuntimeInner {
            event_tx,
            command_tx,
            agent_manager,
            provider_registry: registry,
            active_streams: Arc::new(Mutex::new(HashMap::new())),
            queued_messages: Arc::new(Mutex::new(HashMap::new())),
            openai_codex_auth,
            provider_config_path,
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

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
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
                    kraai_provider_core::ProviderError::ConfigParseError(error.to_string())
                })
            },
            OpenAiCodexFactory::validate_provider_config,
            OpenAiCodexFactory::validate_model_config,
        )
        .map_err(|error| eyre!(error.to_string()))?;
    Ok(registry)
}

async fn execute_tool_requests(
    executions: Vec<ToolExecutionRequest>,
) -> Vec<kraai_types::ToolResult> {
    let mut results = Vec::with_capacity(executions.len());

    for execution in executions {
        let (output, permission_denied, tool_state_deltas) = match execution.payload {
            ToolExecutionPayload::Denied => (
                serde_json::json!({ "error": "Permission denied by user" }),
                true,
                Vec::new(),
            ),
            ToolExecutionPayload::Approved {
                prepared,
                config,
                tool_state_snapshot,
            } => {
                let ctx = ToolContext {
                    global_config: &config,
                    tool_state_snapshot: &tool_state_snapshot,
                };
                let result = prepared.call(&ctx).await;
                match result.output {
                    ToolOutput::Success { data } => (data, false, result.tool_state_deltas),
                    ToolOutput::Error { message } => {
                        (serde_json::json!({ "error": message }), false, Vec::new())
                    }
                }
            }
        };

        results.push(kraai_types::ToolResult {
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
    event_tx: broadcast::Sender<Event>,
    command_tx: mpsc::Sender<Command>,
    agent_manager: Arc<Mutex<AgentManager>>,
    provider_registry: ProviderRegistry,
    active_streams: Arc<Mutex<HashMap<String, ActiveStream>>>,
    queued_messages: Arc<Mutex<HashMap<String, VecDeque<QueuedMessage>>>>,
    openai_codex_auth: Arc<OpenAiCodexAuthController>,
    provider_config_path: PathBuf,
}

#[derive(Clone, Debug)]
struct ActiveStream {
    message_id: MessageId,
    abort_handle: AbortHandle,
}

#[derive(Clone, Debug)]
struct QueuedMessage {
    message: String,
    model_id: ModelId,
    provider_id: ProviderId,
    auto_approve: bool,
}

#[derive(Clone)]
struct RuntimeServices {
    event_tx: broadcast::Sender<Event>,
    command_tx: mpsc::Sender<Command>,
    agent_manager: Arc<Mutex<AgentManager>>,
    active_streams: Arc<Mutex<HashMap<String, ActiveStream>>>,
    queued_messages: Arc<Mutex<HashMap<String, VecDeque<QueuedMessage>>>>,
}

#[derive(Debug)]
enum StreamDriveResult {
    Completed { session_id: String, content: String },
    FailedToStart { error: String },
    FailedDuringStream { error: String },
    Stopped,
}

const TOOL_CALL_OPEN_TAG: &str = "<tool_call>";
const TOOL_CALL_CLOSE_TAG: &str = "</tool_call>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ToolCallStreamPhase {
    #[default]
    PrefixVisible,
    PrefixInsideThink,
    InsideToolCall,
    AfterToolCall,
}

#[derive(Debug, Default)]
struct ToolCallStreamGuard {
    phase: ToolCallStreamPhase,
    buffer: String,
}

#[derive(Debug, Default)]
struct ToolCallGuardChunkResult {
    accepted: String,
    should_stop: bool,
}

impl ToolCallStreamGuard {
    fn ingest_chunk(&mut self, chunk: &str) -> ToolCallGuardChunkResult {
        self.buffer.push_str(chunk);

        let mut accepted = String::new();
        let mut cursor = 0usize;
        let mut should_stop = false;

        while cursor < self.buffer.len() {
            let remaining = &self.buffer[cursor..];

            match self.phase {
                ToolCallStreamPhase::PrefixVisible => {
                    if remaining.starts_with(TOOL_CALL_OPEN_TAG) {
                        accepted.push_str(TOOL_CALL_OPEN_TAG);
                        cursor += TOOL_CALL_OPEN_TAG.len();
                        self.phase = ToolCallStreamPhase::InsideToolCall;
                        continue;
                    }

                    if is_partial_prefix(remaining, TOOL_CALL_OPEN_TAG)
                        || is_possible_open_think_tag_prefix(remaining)
                    {
                        break;
                    }

                    if let Some(tag) = parse_full_think_tag_at_start(remaining) {
                        accepted.push_str(&remaining[..tag.len]);
                        cursor += tag.len;
                        if !tag.closing {
                            self.phase = ToolCallStreamPhase::PrefixInsideThink;
                        }
                        continue;
                    }

                    let ch = remaining
                        .chars()
                        .next()
                        .expect("remaining content should have a character");
                    accepted.push(ch);
                    cursor += ch.len_utf8();
                }
                ToolCallStreamPhase::PrefixInsideThink => {
                    if let Some(tag) = parse_full_think_tag_at_start(remaining) {
                        accepted.push_str(&remaining[..tag.len]);
                        cursor += tag.len;
                        if tag.closing {
                            self.phase = ToolCallStreamPhase::PrefixVisible;
                        }
                        continue;
                    }

                    if is_possible_close_think_tag_prefix(remaining) {
                        break;
                    }

                    let ch = remaining
                        .chars()
                        .next()
                        .expect("remaining content should have a character");
                    accepted.push(ch);
                    cursor += ch.len_utf8();
                }
                ToolCallStreamPhase::InsideToolCall => {
                    if let Some(close_index) = remaining.find(TOOL_CALL_CLOSE_TAG) {
                        let close_end = close_index + TOOL_CALL_CLOSE_TAG.len();
                        accepted.push_str(&remaining[..close_end]);
                        cursor += close_end;
                        self.phase = ToolCallStreamPhase::AfterToolCall;
                        continue;
                    }

                    let keep_len = partial_suffix_len(remaining, TOOL_CALL_CLOSE_TAG);
                    let safe_len = remaining.len().saturating_sub(keep_len);
                    if safe_len == 0 {
                        break;
                    }

                    accepted.push_str(&remaining[..safe_len]);
                    cursor += safe_len;
                }
                ToolCallStreamPhase::AfterToolCall => {
                    let whitespace_len = remaining
                        .chars()
                        .take_while(|ch| ch.is_whitespace())
                        .map(char::len_utf8)
                        .sum();
                    if whitespace_len > 0 {
                        accepted.push_str(&remaining[..whitespace_len]);
                        cursor += whitespace_len;
                        continue;
                    }

                    if remaining.starts_with(TOOL_CALL_OPEN_TAG) {
                        accepted.push_str(TOOL_CALL_OPEN_TAG);
                        cursor += TOOL_CALL_OPEN_TAG.len();
                        self.phase = ToolCallStreamPhase::InsideToolCall;
                        continue;
                    }

                    if is_partial_prefix(remaining, TOOL_CALL_OPEN_TAG) {
                        break;
                    }

                    should_stop = true;
                    break;
                }
            }
        }

        if should_stop {
            self.buffer.clear();
        } else {
            self.buffer.drain(..cursor);
        }

        ToolCallGuardChunkResult {
            accepted,
            should_stop,
        }
    }

    fn finish(&mut self) -> String {
        match self.phase {
            ToolCallStreamPhase::AfterToolCall => {
                self.buffer.clear();
                String::new()
            }
            ToolCallStreamPhase::PrefixVisible
            | ToolCallStreamPhase::PrefixInsideThink
            | ToolCallStreamPhase::InsideToolCall => std::mem::take(&mut self.buffer),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedThinkTag {
    len: usize,
    closing: bool,
}

fn is_partial_prefix(input: &str, pattern: &str) -> bool {
    !input.is_empty() && input.len() < pattern.len() && pattern.starts_with(input)
}

fn partial_suffix_len(input: &str, pattern: &str) -> usize {
    let max_len = input.len().min(pattern.len().saturating_sub(1));
    for len in (1..=max_len).rev() {
        if input.ends_with(&pattern[..len]) {
            return len;
        }
    }
    0
}

fn parse_full_think_tag_at_start(input: &str) -> Option<ParsedThinkTag> {
    if !input.starts_with('<') {
        return None;
    }

    let bytes = input.as_bytes();
    let mut cursor = 1usize;
    let closing = matches!(bytes.get(cursor), Some(b'/'));
    if closing {
        cursor += 1;
    }

    let name_len = if input[cursor..].len() >= "thinking".len()
        && input[cursor..cursor + "thinking".len()].eq_ignore_ascii_case("thinking")
    {
        "thinking".len()
    } else if input[cursor..].len() >= "think".len()
        && input[cursor..cursor + "think".len()].eq_ignore_ascii_case("think")
    {
        "think".len()
    } else {
        return None;
    };
    cursor += name_len;

    let next = input[cursor..].chars().next()?;
    if next.is_ascii_alphanumeric() || next == '_' {
        return None;
    }

    let close_len = input[cursor..].find('>')?;
    Some(ParsedThinkTag {
        len: cursor + close_len + 1,
        closing,
    })
}

fn is_possible_open_think_tag_prefix(input: &str) -> bool {
    is_possible_think_tag_prefix(input, false)
}

fn is_possible_close_think_tag_prefix(input: &str) -> bool {
    is_possible_think_tag_prefix(input, true)
}

fn is_possible_think_tag_prefix(input: &str, closing: bool) -> bool {
    if !input.starts_with('<') || input.contains('>') {
        return false;
    }

    let bytes = input.as_bytes();
    let mut cursor = 1usize;

    if closing {
        if bytes.get(cursor) != Some(&b'/') {
            return false;
        }
        cursor += 1;
    } else if bytes.get(cursor) == Some(&b'/') {
        return false;
    }

    let name = &input[cursor..];
    if name.is_empty() {
        return true;
    }

    let letters_len = name
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic())
        .count();
    let letters = &name[..letters_len];

    let matches_prefix = ["think", "thinking"]
        .iter()
        .any(|candidate| candidate.starts_with(&letters.to_ascii_lowercase()));
    if !matches_prefix {
        return false;
    }

    if letters_len == name.len() {
        return true;
    }

    let remainder = &name[letters_len..];
    let next = remainder
        .chars()
        .next()
        .expect("remainder should contain a character");
    let matched_full_name = ["think", "thinking"]
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(letters));

    matched_full_name && !(next.is_ascii_alphanumeric() || next == '_')
}

impl RuntimeInner {
    fn send_event(&self, event: Event) {
        emit_event(&self.event_tx, event);
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
                let settings =
                    read_settings_document(&self.provider_config_path, &self.provider_registry)?;
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
                auto_approve,
            } => {
                self.handle_send_message(session_id, message, model_id, provider_id, auto_approve)
                    .await;
            }

            Command::StartQueuedMessages { session_id } => {
                self.handle_start_queued_messages(session_id).await;
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
                        ..Session::from_session_meta(session)
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
                self.queued_messages.lock().await.remove(&session_id);
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

            Command::UndoLastUserMessage {
                session_id,
                response,
            } => {
                let restored_message = self
                    .agent_manager
                    .lock()
                    .await
                    .undo_last_user_message(&session_id)
                    .await?;
                response
                    .send(restored_message)
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
                let call_id = kraai_types::CallId::new(call_id);
                self.agent_manager
                    .lock()
                    .await
                    .approve_tool(&session_id, call_id);
            }

            Command::DenyTool {
                session_id,
                call_id,
            } => {
                let call_id = kraai_types::CallId::new(call_id);
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

            Command::ContinueSession { session_id } => {
                Self::start_continuation(
                    session_id,
                    RuntimeServices {
                        event_tx: self.event_tx.clone(),
                        command_tx: self.command_tx.clone(),
                        agent_manager: self.agent_manager.clone(),
                        active_streams: self.active_streams.clone(),
                        queued_messages: self.queued_messages.clone(),
                    },
                )
                .await;
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
        auto_approve: bool,
    ) {
        let has_queued_messages = {
            let queued = self.queued_messages.lock().await;
            queued
                .get(&session_id)
                .is_some_and(|queue| !queue.is_empty())
        };
        let is_turn_active = {
            let agent = self.agent_manager.lock().await;
            agent.is_turn_active(&session_id)
        };
        if is_turn_active || has_queued_messages {
            self.enqueue_message(
                &session_id,
                QueuedMessage {
                    message,
                    model_id,
                    provider_id,
                    auto_approve,
                },
            )
            .await;
            Self::schedule_queue_drain(&session_id, self.command_tx.clone()).await;
            return;
        }

        let stream_request = {
            let mut agent = self.agent_manager.lock().await;
            match agent
                .prepare_start_stream_with_options(
                    &session_id,
                    message,
                    model_id,
                    provider_id,
                    auto_approve,
                )
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
            Self::schedule_queue_drain(&session_id, self.command_tx.clone()).await;
            return;
        };

        self.spawn_stream_task(session_id, providers, request).await;
    }

    async fn enqueue_message(&self, session_id: &str, queued_message: QueuedMessage) {
        let mut queued = self.queued_messages.lock().await;
        queued
            .entry(session_id.to_string())
            .or_default()
            .push_back(queued_message);
    }

    async fn handle_start_queued_messages(&self, session_id: String) {
        let is_turn_active = {
            let agent = self.agent_manager.lock().await;
            agent.is_turn_active(&session_id)
        };
        if is_turn_active {
            return;
        }

        loop {
            let next_message = {
                let mut queued = self.queued_messages.lock().await;
                let Some(queue) = queued.get_mut(&session_id) else {
                    return;
                };
                let next = queue.pop_front();
                if queue.is_empty() {
                    queued.remove(&session_id);
                }
                next
            };

            let Some(next_message) = next_message else {
                return;
            };

            let stream_request = {
                let mut agent = self.agent_manager.lock().await;
                match agent
                    .prepare_start_stream_with_options(
                        &session_id,
                        next_message.message,
                        next_message.model_id,
                        next_message.provider_id,
                        next_message.auto_approve,
                    )
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
                continue;
            };

            self.spawn_stream_task(session_id, providers, request).await;
            return;
        }
    }

    async fn schedule_queue_drain(session_id: &str, command_tx: mpsc::Sender<Command>) {
        let _ = command_tx
            .send(Command::StartQueuedMessages {
                session_id: session_id.to_string(),
            })
            .await;
    }

    async fn handle_completion_persistence_failure(
        session_id: String,
        message_id: MessageId,
        error: color_eyre::Report,
        continuation_error: bool,
        command_tx: mpsc::Sender<Command>,
        agent_manager: Arc<Mutex<AgentManager>>,
        event_tx: broadcast::Sender<Event>,
    ) {
        let rollback_result = {
            let mut agent = agent_manager.lock().await;
            let rollback_result = agent.abort_streaming_message(&message_id).await;
            if rollback_result.is_ok() {
                agent.clear_active_turn(&session_id);
            }
            rollback_result
        };

        match rollback_result {
            Ok(Some(_)) => {
                Self::schedule_queue_drain(&session_id, command_tx).await;
                emit_event(
                    &event_tx,
                    Event::HistoryUpdated {
                        session_id: session_id.clone(),
                    },
                );
            }
            Ok(None) => {
                emit_event(
                    &event_tx,
                    Event::Error(format!(
                        "Failed to recover stream state for message {} after completion error",
                        message_id
                    )),
                );
            }
            Err(rollback_error) => {
                emit_event(
                    &event_tx,
                    Event::Error(format!(
                        "Failed to roll back stream {} after completion error: {rollback_error}",
                        message_id
                    )),
                );
            }
        }

        if continuation_error {
            emit_event(
                &event_tx,
                Event::ContinuationFailed {
                    session_id,
                    error: error.to_string(),
                },
            );
        } else {
            emit_event(
                &event_tx,
                Event::StreamError {
                    session_id,
                    message_id: message_id.to_string(),
                    error: error.to_string(),
                },
            );
        }
    }

    async fn abort_stream_for_recovery(
        session_id: &str,
        message_id: &MessageId,
        agent_manager: Arc<Mutex<AgentManager>>,
    ) -> Result<bool> {
        let mut agent = agent_manager.lock().await;
        let rollback_result = agent.abort_streaming_message(message_id).await?;
        if rollback_result.is_some() {
            agent.clear_active_turn(session_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn handle_execute_tools(&self, session_id: String) {
        let event_tx = self.event_tx.clone();
        let agent_manager = self.agent_manager.clone();
        let command_tx = self.command_tx.clone();
        let active_streams = self.active_streams.clone();
        let queued_messages = self.queued_messages.clone();
        let services = RuntimeServices {
            event_tx: event_tx.clone(),
            command_tx: command_tx.clone(),
            agent_manager: agent_manager.clone(),
            active_streams: active_streams.clone(),
            queued_messages: queued_messages.clone(),
        };

        tokio::spawn(async move {
            let executions = {
                let mut agent = agent_manager.lock().await;
                agent.take_ready_tool_executions(&session_id)
            };
            let executed_source_message_ids: Vec<_> = executions
                .iter()
                .map(|execution| execution.source_message_id.clone())
                .collect();
            let mut completed_source_message_ids = Vec::new();
            for execution in &executions {
                if !completed_source_message_ids.contains(&execution.source_message_id) {
                    completed_source_message_ids.push(execution.source_message_id.clone());
                }
            }

            let results = execute_tool_requests(executions).await;

            for result in &results {
                let success = result.output.get("error").is_none();
                let output = serde_json::to_string(&result.output).unwrap_or_default();

                emit_event(
                    &event_tx,
                    Event::ToolResultReady {
                        session_id: session_id.clone(),
                        call_id: result.call_id.to_string(),
                        tool_id: result.tool_id.to_string(),
                        success,
                        output,
                        denied: result.permission_denied,
                    },
                );
            }

            // Add results to history
            {
                let mut agent = agent_manager.lock().await;
                if let Err(error) = agent
                    .add_tool_results_to_history(&session_id, results)
                    .await
                {
                    agent.clear_active_turn(&session_id);
                    drop(agent);
                    emit_event(&event_tx, Event::Error(error.to_string()));
                    emit_event(
                        &event_tx,
                        Event::ContinuationFailed {
                            session_id: session_id.clone(),
                            error: error.to_string(),
                        },
                    );
                    emit_event(
                        &event_tx,
                        Event::HistoryUpdated {
                            session_id: session_id.clone(),
                        },
                    );
                    Self::schedule_queue_drain(&session_id, command_tx.clone()).await;
                    return;
                }
                agent.finish_tool_executions(&session_id, &executed_source_message_ids);
            }

            tracing::debug!("Emitting HistoryUpdated event after tool results");
            emit_event(
                &event_tx,
                Event::HistoryUpdated {
                    session_id: session_id.clone(),
                },
            );

            for source_message_id in completed_source_message_ids {
                let has_pending_tools = {
                    agent_manager
                        .lock()
                        .await
                        .has_unfinished_tools_for_message(&session_id, &source_message_id)
                };
                if has_pending_tools {
                    continue;
                }

                Self::start_continuation(session_id.clone(), services.clone()).await;
            }
        });
    }

    fn start_continuation(
        session_id: String,
        services: RuntimeServices,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            let agent_manager = services.agent_manager.clone();
            let event_tx = services.event_tx.clone();
            let command_tx = services.command_tx.clone();
            let active_streams = services.active_streams.clone();
            let queued_messages = services.queued_messages.clone();
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
                    Self::schedule_queue_drain(&session_id, command_tx.clone()).await;
                    emit_event(
                        &event_tx,
                        Event::HistoryUpdated {
                            session_id: session_id.clone(),
                        },
                    );
                    emit_event(
                        &event_tx,
                        Event::ContinuationFailed {
                            session_id,
                            error: error.to_string(),
                        },
                    );
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
                        event_tx.clone(),
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
                            let completed_session = match completed_session {
                                Ok(Some(completed_session)) => completed_session,
                                Ok(None) => return,
                                Err(error) => {
                                    Self::handle_completion_persistence_failure(
                                        request_session_id.clone(),
                                        request_message_id.clone(),
                                        error,
                                        true,
                                        command_tx.clone(),
                                        agent_manager.clone(),
                                        event_tx.clone(),
                                    )
                                    .await;
                                    return;
                                }
                            };

                            emit_event(
                                &event_tx,
                                Event::StreamComplete {
                                    session_id: completed_session.clone(),
                                    message_id: request_message_id.to_string(),
                                },
                            );
                            emit_event(
                                &event_tx,
                                Event::HistoryUpdated {
                                    session_id: completed_session.clone(),
                                },
                            );
                            Self::process_completed_stream_output(
                                completed_session,
                                request_message_id,
                                content,
                                RuntimeServices {
                                    event_tx: event_tx.clone(),
                                    command_tx: command_tx.clone(),
                                    agent_manager: agent_manager.clone(),
                                    active_streams: task_active_streams.clone(),
                                    queued_messages: queued_messages.clone(),
                                },
                            )
                            .await;
                        }
                        StreamDriveResult::FailedToStart { error } => {
                            match Self::abort_stream_for_recovery(
                                &request_session_id,
                                &request_message_id,
                                agent_manager.clone(),
                            )
                            .await
                            {
                                Ok(true) => {
                                    Self::schedule_queue_drain(
                                        &request_session_id,
                                        command_tx.clone(),
                                    )
                                    .await;
                                    emit_event(
                                        &event_tx,
                                        Event::HistoryUpdated {
                                            session_id: request_session_id.clone(),
                                        },
                                    );
                                }
                                Ok(false) => {
                                    emit_event(
                                        &event_tx,
                                        Event::Error(format!(
                                            "Failed to recover continuation stream {} after start failure",
                                            request_message_id
                                        )),
                                    );
                                }
                                Err(rollback_error) => {
                                    emit_event(
                                        &event_tx,
                                        Event::Error(format!(
                                            "Failed to roll back continuation stream {} after start failure: {rollback_error}",
                                            request_message_id
                                        )),
                                    );
                                }
                            }
                            emit_event(
                                &event_tx,
                                Event::ContinuationFailed {
                                    session_id: request_session_id,
                                    error,
                                },
                            );
                        }
                        StreamDriveResult::FailedDuringStream { error } => {
                            match Self::abort_stream_for_recovery(
                                &request_session_id,
                                &request_message_id,
                                agent_manager.clone(),
                            )
                            .await
                            {
                                Ok(true) => {
                                    Self::schedule_queue_drain(
                                        &request_session_id,
                                        command_tx.clone(),
                                    )
                                    .await;
                                }
                                Ok(false) => {
                                    emit_event(
                                        &event_tx,
                                        Event::Error(format!(
                                            "Failed to recover continuation stream {} after runtime error",
                                            request_message_id
                                        )),
                                    );
                                }
                                Err(rollback_error) => {
                                    emit_event(
                                        &event_tx,
                                        Event::Error(format!(
                                            "Failed to roll back continuation stream {} after runtime error: {rollback_error}",
                                            request_message_id
                                        )),
                                    );
                                }
                            }
                            tracing::error!("Continuation stream error: {}", error);
                            emit_event(
                                &event_tx,
                                Event::StreamError {
                                    session_id: request_session_id,
                                    message_id: request_message_id.to_string(),
                                    error,
                                },
                            );
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
        let event_tx = self.event_tx.clone();
        let agent_manager = self.agent_manager.clone();
        let command_tx = self.command_tx.clone();
        let active_streams = self.active_streams.clone();
        let queued_messages = self.queued_messages.clone();
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
                    event_tx.clone(),
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
                        let completed_session = match completed_session {
                            Ok(Some(completed_session)) => completed_session,
                            Ok(None) => return,
                            Err(error) => {
                                Self::handle_completion_persistence_failure(
                                    request_session_id.clone(),
                                    request_message_id.clone(),
                                    error,
                                    false,
                                    command_tx.clone(),
                                    agent_manager.clone(),
                                    event_tx.clone(),
                                )
                                .await;
                                return;
                            }
                        };

                        emit_event(
                            &event_tx,
                            Event::StreamComplete {
                                session_id: completed_session.clone(),
                                message_id: request_message_id.to_string(),
                            },
                        );
                        emit_event(
                            &event_tx,
                            Event::HistoryUpdated {
                                session_id: completed_session.clone(),
                            },
                        );
                        Self::process_completed_stream_output(
                            completed_session,
                            request_message_id,
                            content,
                            RuntimeServices {
                                event_tx: event_tx.clone(),
                                command_tx: command_tx.clone(),
                                agent_manager: agent_manager.clone(),
                                active_streams: task_active_streams.clone(),
                                queued_messages: queued_messages.clone(),
                            },
                        )
                        .await;
                    }
                    StreamDriveResult::FailedToStart { error } => {
                        match Self::abort_stream_for_recovery(
                            &request_session_id,
                            &request_message_id,
                            agent_manager.clone(),
                        )
                        .await
                        {
                            Ok(true) => {
                                Self::schedule_queue_drain(&request_session_id, command_tx.clone())
                                    .await;
                            }
                            Ok(false) => {
                                emit_event(
                                    &event_tx,
                                    Event::Error(format!(
                                        "Failed to recover stream {} after start failure",
                                        request_message_id
                                    )),
                                );
                            }
                            Err(rollback_error) => {
                                emit_event(
                                    &event_tx,
                                    Event::Error(format!(
                                        "Failed to roll back stream {} after start failure: {rollback_error}",
                                        request_message_id
                                    )),
                                );
                            }
                        }
                        emit_event(&event_tx, Event::Error(error));
                    }
                    StreamDriveResult::FailedDuringStream { error } => {
                        match Self::abort_stream_for_recovery(
                            &request_session_id,
                            &request_message_id,
                            agent_manager.clone(),
                        )
                        .await
                        {
                            Ok(true) => {
                                Self::schedule_queue_drain(&request_session_id, command_tx.clone())
                                    .await;
                            }
                            Ok(false) => {
                                emit_event(
                                    &event_tx,
                                    Event::Error(format!(
                                        "Failed to recover stream {} after runtime error",
                                        request_message_id
                                    )),
                                );
                            }
                            Err(rollback_error) => {
                                emit_event(
                                    &event_tx,
                                    Event::Error(format!(
                                        "Failed to roll back stream {} after runtime error: {rollback_error}",
                                        request_message_id
                                    )),
                                );
                            }
                        }
                        emit_event(
                            &event_tx,
                            Event::StreamError {
                                session_id: request_session_id,
                                message_id: request_message_id.to_string(),
                                error,
                            },
                        );
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
        event_tx: broadcast::Sender<Event>,
    ) -> StreamDriveResult {
        let PendingStreamRequest {
            message_id: msg_id,
            provider_id,
            model_id,
            provider_messages,
        } = request;
        let request_context =
            ProviderRequestContext::with_retry_observer(Arc::new(RuntimeRetryObserver {
                session_id: session_id.clone(),
                provider_id: provider_id.clone(),
                model_id: model_id.clone(),
                event_tx: event_tx.clone(),
            }));
        let mut stream = match providers
            .generate_reply_stream(provider_id, &model_id, provider_messages, request_context)
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                return StreamDriveResult::FailedToStart {
                    error: error.to_string(),
                };
            }
        };

        emit_event(
            &event_tx,
            Event::StreamStart {
                session_id: session_id.clone(),
                message_id: msg_id.to_string(),
            },
        );

        let mut content = String::new();
        let mut guard = ToolCallStreamGuard::default();

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    let guarded = guard.ingest_chunk(&chunk);
                    if !guarded.accepted.is_empty() {
                        content.push_str(&guarded.accepted);
                        {
                            let agent = agent_manager.lock().await;
                            if !agent.append_chunk(&msg_id, &guarded.accepted).await {
                                return StreamDriveResult::Stopped;
                            }
                        }
                        emit_event(
                            &event_tx,
                            Event::StreamChunk {
                                session_id: session_id.clone(),
                                message_id: msg_id.to_string(),
                                chunk: guarded.accepted,
                            },
                        );
                    }
                    if guarded.should_stop {
                        tracing::debug!(
                            "Stopping stream after invalid content following tool call"
                        );
                        return StreamDriveResult::Completed {
                            session_id,
                            content,
                        };
                    }
                }
                Err(error) => {
                    return StreamDriveResult::FailedDuringStream {
                        error: error.to_string(),
                    };
                }
            }
        }

        let tail = guard.finish();
        if !tail.is_empty() {
            content.push_str(&tail);
            {
                let agent = agent_manager.lock().await;
                if !agent.append_chunk(&msg_id, &tail).await {
                    return StreamDriveResult::Stopped;
                }
            }
            emit_event(
                &event_tx,
                Event::StreamChunk {
                    session_id: session_id.clone(),
                    message_id: msg_id.to_string(),
                    chunk: tail,
                },
            );
        }

        tracing::debug!("Full content length: {}", content.len());

        StreamDriveResult::Completed {
            session_id,
            content,
        }
    }

    async fn process_completed_stream_output(
        completed_session: String,
        source_message_id: MessageId,
        content: String,
        services: RuntimeServices,
    ) {
        let agent_manager = services.agent_manager.clone();
        let event_tx = services.event_tx.clone();
        let command_tx = services.command_tx.clone();
        let (tool_calls, failed) = {
            let mut agent = agent_manager.lock().await;
            match agent
                .parse_tool_calls_from_content(&completed_session, &source_message_id, &content)
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    {
                        let mut agent = agent_manager.lock().await;
                        agent.clear_active_turn(&completed_session);
                    }
                    Self::schedule_queue_drain(&completed_session, command_tx.clone()).await;
                    emit_event(
                        &event_tx,
                        Event::HistoryUpdated {
                            session_id: completed_session,
                        },
                    );
                    emit_event(&event_tx, Event::Error(error.to_string()));
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
            let add_result = {
                let mut agent = agent_manager.lock().await;
                agent
                    .add_parse_failures_to_history(&completed_session, failed)
                    .await
            };
            if let Err(error) = add_result {
                {
                    let mut agent = agent_manager.lock().await;
                    agent.clear_active_turn(&completed_session);
                }
                Self::schedule_queue_drain(&completed_session, command_tx.clone()).await;
                emit_event(&event_tx, Event::Error(error.to_string()));
                emit_event(
                    &event_tx,
                    Event::ContinuationFailed {
                        session_id: completed_session,
                        error: error.to_string(),
                    },
                );
                return;
            }
            emit_event(
                &event_tx,
                Event::HistoryUpdated {
                    session_id: completed_session.clone(),
                },
            );
            Self::start_continuation(completed_session, services).await;
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
                emit_event(
                    &event_tx,
                    Event::ToolCallDetected {
                        session_id: completed_session.clone(),
                        call_id: tool_call.call_id.to_string(),
                        tool_id: tool_call.tool_id,
                        args: args_json,
                        description: tool_call.description,
                        risk_level: tool_call.assessment.risk.as_str().to_string(),
                        reasons: tool_call.assessment.reasons,
                        queue_order: tool_call.queue_order,
                    },
                );
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
            Self::schedule_queue_drain(&completed_session, command_tx).await;
            emit_event(
                &event_tx,
                Event::HistoryUpdated {
                    session_id: completed_session,
                },
            );
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
        Self::schedule_queue_drain(&session_id, self.command_tx.clone()).await;
        Ok(true)
    }

    fn spawn_config_watcher(&self) {
        let command_tx = self.command_tx.clone();
        let event_tx = self.event_tx.clone();
        let config_loc = self.provider_config_path.clone();

        tokio::spawn(async move {
            let config_dir = match config_loc.parent() {
                Some(path) => path.to_path_buf(),
                None => {
                    emit_event(
                        &event_tx,
                        Event::Error(String::from("Config path has no parent")),
                    );
                    return;
                }
            };
            if let Err(error) = std::fs::create_dir_all(&config_dir) {
                emit_event(
                    &event_tx,
                    Event::Error(format!(
                        "Failed to create config directory {}: {error}",
                        config_dir.display()
                    )),
                );
                return;
            }

            let (tx, rx) = std::sync::mpsc::channel();
            let mut watcher = match notify::recommended_watcher(tx) {
                Ok(watcher) => watcher,
                Err(error) => {
                    emit_event(
                        &event_tx,
                        Event::Error(format!("Failed to create config watcher: {error}")),
                    );
                    return;
                }
            };

            if let Err(error) = watcher.watch(&config_dir, RecursiveMode::NonRecursive) {
                emit_event(
                    &event_tx,
                    Event::Error(format!(
                        "Failed to watch config directory {}: {error}",
                        config_dir.display()
                    )),
                );
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
                        emit_event(
                            &event_tx,
                            Event::Error(format!("Config watch error: {:?}", e)),
                        );
                    }
                }
            }
        });
    }

    async fn load_providers_config(&self) -> Result<()> {
        let config_loc = &self.provider_config_path;
        let config = if !config_loc.exists() {
            ProviderManagerConfig {
                providers: Vec::new(),
                models: Vec::new(),
            }
        } else {
            let config_slice = tokio::fs::read(&config_loc).await?;
            toml::from_slice(&config_slice).wrap_err_with(|| {
                format!("Failed to parse provider config {}", config_loc.display())
            })?
        };

        self.agent_manager
            .lock()
            .await
            .set_providers(config, self.provider_registry.clone())
            .await?;

        Ok(())
    }

    async fn save_settings_document(&self, settings: SettingsDocument) -> Result<()> {
        write_settings_document(
            &self.provider_config_path,
            &settings,
            &self.provider_registry,
        )
        .await?;
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::EventCallback;
    use crate::settings::{default_provider_config_path, resolve_provider_config_path};
    use async_trait::async_trait;
    use color_eyre::eyre::{Result, eyre};
    use futures::stream::{self, BoxStream};
    use kraai_agent::AgentManager;
    use kraai_persistence::{
        FileMessageStore, FileSessionStore, MessageStore, SessionMeta, SessionStore,
    };
    use kraai_provider_core::ModelConfig;
    use kraai_tool_core::{ToolCallResult, ToolContext, ToolManager, TypedTool};
    use kraai_tool_edit_file::EditFileTool;
    use kraai_tool_open_file::OpenFileTool;
    use kraai_tool_read_file::ReadFileTool;
    use kraai_types::{
        ChatMessage, ChatRole, ExecutionPolicy, MessageStatus, ModelId, ProviderId, RiskLevel,
        ToolCallAssessment,
    };
    use serde::Deserialize;

    macro_rules! runtime_harness_or_skip {
        ($expr:expr) => {
            match $expr.await {
                Some(harness) => harness,
                None => return Ok(()),
            }
        };
    }

    fn is_missing_system_ca_error(error: &dyn std::error::Error) -> bool {
        let mut current = Some(error);
        while let Some(error) = current {
            let display = error.to_string();
            let debug = format!("{error:?}");
            if display.contains("No CA certificates were loaded from the system")
                || debug.contains("No CA certificates were loaded from the system")
                || display == "builder error"
            {
                return true;
            }
            current = error.source();
        }
        false
    }

    #[test]
    fn resolve_provider_config_path_uses_override_when_present() {
        let override_path = PathBuf::from("/tmp/custom-providers.toml");

        let resolved =
            resolve_provider_config_path(Some(override_path.clone())).expect("path should resolve");

        assert_eq!(resolved, override_path);
    }

    #[test]
    fn resolve_provider_config_path_falls_back_to_default_location() {
        let resolved = resolve_provider_config_path(None).expect("default path should resolve");

        assert_eq!(
            resolved,
            default_provider_config_path().expect("default path should resolve")
        );
    }

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
    impl kraai_provider_core::Provider for ScriptedProvider {
        fn get_provider_id(&self) -> ProviderId {
            self.id.clone()
        }

        async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
            vec![kraai_provider_core::Model {
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
            _request_context: &kraai_provider_core::ProviderRequestContext,
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
            _request_context: &kraai_provider_core::ProviderRequestContext,
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

        async fn call(&self, args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
            ToolCallResult::success(serde_json::json!({
                "tool": "mock_tool",
                "value": args.value,
            }))
        }

        fn describe(&self, args: &Self::Args) -> String {
            format!("Mock tool for {}", args.value)
        }
    }

    #[derive(Clone, Copy)]
    struct AutonomousTool;

    #[async_trait]
    impl TypedTool for AutonomousTool {
        type Args = ValueArgs;

        fn name(&self) -> &'static str {
            "auto_tool"
        }

        fn schema(&self) -> &'static str {
            "auto_tool(value: string)"
        }

        fn assess(&self, args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
            ToolCallAssessment {
                risk: RiskLevel::ReadOnlyWorkspace,
                policy: ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace),
                reasons: vec![format!(
                    "auto_tool can run autonomously for {:?}",
                    args.value
                )],
            }
        }

        async fn call(&self, args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
            ToolCallResult::success(serde_json::json!({
                "tool": "auto_tool",
                "value": args.value,
            }))
        }

        fn describe(&self, args: &Self::Args) -> String {
            format!("Autonomous tool for {}", args.value)
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

        async fn call(&self, args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
            self.started.notify_waiters();
            self.release.notified().await;

            if let Some(message) = &self.fail_message {
                ToolCallResult::error(message.clone())
            } else {
                ToolCallResult::success(serde_json::json!({
                    "tool": "blocking_tool",
                    "value": args.value,
                }))
            }
        }

        fn describe(&self, args: &Self::Args) -> String {
            format!("Blocking tool for {}", args.value)
        }
    }

    #[derive(Clone)]
    struct BatchBlockingApprovalTool {
        started: Arc<tokio::sync::Notify>,
        ready: Arc<tokio::sync::Barrier>,
    }

    #[async_trait]
    impl TypedTool for BatchBlockingApprovalTool {
        type Args = ValueArgs;

        fn name(&self) -> &'static str {
            "batch_blocking_tool"
        }

        fn schema(&self) -> &'static str {
            "batch_blocking_tool(value: string)"
        }

        fn assess(&self, args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
            ToolCallAssessment {
                risk: RiskLevel::UndoableWorkspaceWrite,
                policy: ExecutionPolicy::AlwaysAsk,
                reasons: vec![format!(
                    "batch_blocking_tool requires approval for {:?}",
                    args.value
                )],
            }
        }

        async fn call(&self, args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
            self.started.notify_waiters();
            self.ready.wait().await;
            if args.value == "beta" {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            ToolCallResult::success(serde_json::json!({
                "tool": "batch_blocking_tool",
                "value": args.value,
            }))
        }

        fn describe(&self, args: &Self::Args) -> String {
            format!("Batch blocking tool for {}", args.value)
        }
    }

    #[derive(Clone, Copy)]
    struct FailingApprovalTool;

    #[async_trait]
    impl TypedTool for FailingApprovalTool {
        type Args = ValueArgs;

        fn name(&self) -> &'static str {
            "failing_tool"
        }

        fn schema(&self) -> &'static str {
            "failing_tool(value: string)"
        }

        fn assess(&self, args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
            ToolCallAssessment {
                risk: RiskLevel::UndoableWorkspaceWrite,
                policy: ExecutionPolicy::AlwaysAsk,
                reasons: vec![format!(
                    "failing_tool requires approval for {:?}",
                    args.value
                )],
            }
        }

        async fn call(&self, _args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
            ToolCallResult::error(String::from("tool exploded"))
        }

        fn describe(&self, args: &Self::Args) -> String {
            format!("Failing tool for {}", args.value)
        }
    }

    struct BlockingStartProvider {
        id: ProviderId,
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    struct DeferredFailingProvider {
        id: ProviderId,
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        failure_message: String,
    }

    struct RetryNotifyingProvider {
        id: ProviderId,
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

        async fn call(&self, _args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
            ToolCallResult::success(serde_json::json!({ "ok": true }))
        }
    }

    #[async_trait]
    impl kraai_provider_core::Provider for BlockingStartProvider {
        fn get_provider_id(&self) -> ProviderId {
            self.id.clone()
        }

        async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
            vec![kraai_provider_core::Model {
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
            _request_context: &kraai_provider_core::ProviderRequestContext,
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
            _request_context: &kraai_provider_core::ProviderRequestContext,
        ) -> Result<BoxStream<'static, Result<String>>> {
            self.started.notify_waiters();
            self.release.notified().await;
            Ok(Box::pin(stream::once(async {
                Ok(String::from("provider started"))
            })))
        }
    }

    #[async_trait]
    impl kraai_provider_core::Provider for DeferredFailingProvider {
        fn get_provider_id(&self) -> ProviderId {
            self.id.clone()
        }

        async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
            vec![kraai_provider_core::Model {
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
            _request_context: &kraai_provider_core::ProviderRequestContext,
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
            _request_context: &kraai_provider_core::ProviderRequestContext,
        ) -> Result<BoxStream<'static, Result<String>>> {
            self.started.notify_waiters();
            self.release.notified().await;
            Err(eyre!(self.failure_message.clone()))
        }
    }

    #[async_trait]
    impl kraai_provider_core::Provider for RetryNotifyingProvider {
        fn get_provider_id(&self) -> ProviderId {
            self.id.clone()
        }

        async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
            vec![kraai_provider_core::Model {
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
            request_context: &kraai_provider_core::ProviderRequestContext,
        ) -> Result<ChatMessage> {
            if let Some(observer) = request_context.retry_observer() {
                observer.on_retry_scheduled(&kraai_provider_core::ProviderRetryEvent {
                    operation: "responses",
                    retry_number: 1,
                    delay: Duration::from_secs(1),
                    reason: String::from("HTTP 429"),
                });
            }

            Ok(ChatMessage {
                role: ChatRole::Assistant,
                content: String::from("unused non-streaming reply"),
            })
        }

        async fn generate_reply_stream(
            &self,
            _model_id: &ModelId,
            _messages: Vec<ChatMessage>,
            request_context: &kraai_provider_core::ProviderRequestContext,
        ) -> Result<BoxStream<'static, Result<String>>> {
            if let Some(observer) = request_context.retry_observer() {
                observer.on_retry_scheduled(&kraai_provider_core::ProviderRetryEvent {
                    operation: "responses",
                    retry_number: 1,
                    delay: Duration::from_secs(1),
                    reason: String::from("HTTP 429"),
                });
            }

            Ok(Box::pin(stream::once(async {
                Ok(String::from("provider started"))
            })))
        }
    }

    #[derive(Clone, Default)]
    struct EventCollector {
        events: Arc<StdMutex<Vec<Event>>>,
    }

    struct FailOnAssistantCompletionMessageStore {
        inner: Arc<dyn MessageStore>,
        should_fail: Arc<AtomicBool>,
    }

    struct FailOnToolMessageStore {
        inner: Arc<dyn MessageStore>,
        should_fail: Arc<AtomicBool>,
    }

    struct FailOnDemandSessionStore {
        inner: Arc<dyn SessionStore>,
        should_fail: Arc<AtomicBool>,
    }

    #[async_trait]
    impl MessageStore for FailOnAssistantCompletionMessageStore {
        async fn get(&self, id: &kraai_types::MessageId) -> Result<Option<kraai_types::Message>> {
            self.inner.get(id).await
        }

        async fn save(&self, message: &kraai_types::Message) -> Result<()> {
            if self.should_fail.load(Ordering::SeqCst)
                && message.role == ChatRole::Assistant
                && message.status == MessageStatus::Complete
            {
                return Err(eyre!("intentional assistant completion save failure"));
            }

            self.inner.save(message).await
        }

        async fn unload(&self, id: &kraai_types::MessageId) {
            self.inner.unload(id).await;
        }

        async fn delete(&self, id: &kraai_types::MessageId) -> Result<()> {
            self.inner.delete(id).await
        }

        async fn exists(&self, id: &kraai_types::MessageId) -> Result<bool> {
            self.inner.exists(id).await
        }

        async fn list_all_on_disk(
            &self,
        ) -> Result<std::collections::HashSet<kraai_types::MessageId>> {
            self.inner.list_all_on_disk().await
        }

        async fn list_hot(&self) -> Result<std::collections::HashSet<kraai_types::MessageId>> {
            self.inner.list_hot().await
        }
    }

    #[async_trait]
    impl MessageStore for FailOnToolMessageStore {
        async fn get(&self, id: &kraai_types::MessageId) -> Result<Option<kraai_types::Message>> {
            self.inner.get(id).await
        }

        async fn save(&self, message: &kraai_types::Message) -> Result<()> {
            if self.should_fail.load(Ordering::SeqCst) && message.role == ChatRole::Tool {
                return Err(eyre!("intentional tool history save failure"));
            }

            self.inner.save(message).await
        }

        async fn unload(&self, id: &kraai_types::MessageId) {
            self.inner.unload(id).await;
        }

        async fn delete(&self, id: &kraai_types::MessageId) -> Result<()> {
            self.inner.delete(id).await
        }

        async fn exists(&self, id: &kraai_types::MessageId) -> Result<bool> {
            self.inner.exists(id).await
        }

        async fn list_all_on_disk(
            &self,
        ) -> Result<std::collections::HashSet<kraai_types::MessageId>> {
            self.inner.list_all_on_disk().await
        }

        async fn list_hot(&self) -> Result<std::collections::HashSet<kraai_types::MessageId>> {
            self.inner.list_hot().await
        }
    }

    #[async_trait]
    impl SessionStore for FailOnDemandSessionStore {
        async fn list(&self) -> Result<Vec<SessionMeta>> {
            self.inner.list().await
        }

        async fn get(&self, id: &str) -> Result<Option<SessionMeta>> {
            self.inner.get(id).await
        }

        async fn save(&self, session: &SessionMeta) -> Result<()> {
            if self.should_fail.load(Ordering::SeqCst) {
                return Err(eyre!("intentional session save failure for {}", session.id));
            }

            self.inner.save(session).await
        }

        async fn delete(&self, id: &str) -> Result<()> {
            self.inner.delete(id).await
        }
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
        event_task: tokio::task::JoinHandle<()>,
        data_dir: PathBuf,
    }

    impl RuntimeTestHarness {
        async fn new(scripts: Vec<Vec<ScriptedChunk>>) -> Option<Self> {
            Self::new_with_tools(scripts, |_| {}).await
        }

        async fn new_with_tools<F>(
            scripts: Vec<Vec<ScriptedChunk>>,
            configure_tools: F,
        ) -> Option<Self>
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
            tools.register_tool(AutonomousTool);
            configure_tools(&mut tools);

            Self::new_with_parts(providers, tools).await
        }

        async fn new_with_parts(
            providers: ProviderManager,
            mut tools: ToolManager,
        ) -> Option<Self> {
            if tools.list_tools().is_empty() {
                tools.register_tool(ApprovalTool);
            }
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let pid = std::process::id();
            let data_dir = std::env::temp_dir().join(format!("kraai-runtime-test-{pid}-{nanos}"));
            tokio::fs::create_dir_all(&data_dir)
                .await
                .expect("create temp runtime dir");

            let workspace_dir = data_dir.join("workspace");
            tokio::fs::create_dir_all(&workspace_dir)
                .await
                .expect("create temp workspace dir");
            tools.register_tool(NoopTool);
            let profile_dir = workspace_dir.join(".kraai");
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

            Self::new_with_stores_and_parts(
                providers,
                tools,
                message_store,
                session_store,
                data_dir,
            )
            .await
        }

        async fn new_with_stores_and_parts(
            providers: ProviderManager,
            mut tools: ToolManager,
            message_store: Arc<dyn MessageStore>,
            session_store: Arc<dyn SessionStore>,
            data_dir: PathBuf,
        ) -> Option<Self> {
            if tools.list_tools().is_empty() {
                tools.register_tool(ApprovalTool);
            }

            let agent_manager = Arc::new(Mutex::new(AgentManager::new(
                providers,
                tools,
                data_dir.join("workspace"),
                message_store,
                session_store,
            )));

            let events = EventCollector::default();
            let (command_tx, mut command_rx) = mpsc::channel(32);
            let (event_tx, _) = broadcast::channel(1024);
            let handle = RuntimeHandle {
                command_tx: command_tx.clone(),
                event_tx: event_tx.clone(),
            };

            let openai_codex_auth = match OpenAiCodexAuthController::new() {
                Ok(controller) => Arc::new(controller),
                Err(error) if is_missing_system_ca_error(&error) => return None,
                Err(error) => panic!("unexpected openai auth controller init error: {error}"),
            };
            let runtime = RuntimeInner {
                event_tx: event_tx.clone(),
                command_tx,
                agent_manager,
                provider_registry: build_provider_registry(openai_codex_auth.clone())
                    .expect("provider registry"),
                active_streams: Arc::new(Mutex::new(HashMap::new())),
                queued_messages: Arc::new(Mutex::new(HashMap::new())),
                openai_codex_auth,
                provider_config_path: data_dir.join("providers.toml"),
            };

            let events_for_task = events.clone();
            let mut event_rx = event_tx.subscribe();
            let event_task = tokio::spawn(async move {
                loop {
                    match event_rx.recv().await {
                        Ok(event) => events_for_task.on_event(event),
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            let runtime_task = tokio::spawn(async move {
                while let Some(command) = command_rx.recv().await {
                    runtime
                        .handle_command(command)
                        .await
                        .expect("runtime command should succeed");
                }
            });

            Some(Self {
                handle,
                events,
                runtime_task,
                event_task,
                data_dir,
            })
        }

        async fn new_with_message_store<F>(
            scripts: Vec<Vec<ScriptedChunk>>,
            configure_store: F,
        ) -> Option<Self>
        where
            F: FnOnce(Arc<dyn MessageStore>) -> Arc<dyn MessageStore>,
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
            tools.register_tool(AutonomousTool);
            tools.register_tool(NoopTool);

            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let pid = std::process::id();
            let data_dir = std::env::temp_dir().join(format!("kraai-runtime-test-{pid}-{nanos}"));
            tokio::fs::create_dir_all(&data_dir)
                .await
                .expect("create temp runtime dir");
            let workspace_dir = data_dir.join("workspace");
            tokio::fs::create_dir_all(&workspace_dir)
                .await
                .expect("create temp workspace dir");
            let profile_dir = workspace_dir.join(".kraai");
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

            let base_store: Arc<dyn MessageStore> = Arc::new(FileMessageStore::new(&data_dir));
            let message_store = configure_store(base_store.clone());
            let session_store: Arc<dyn SessionStore> =
                Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));

            Self::new_with_stores_and_parts(
                providers,
                tools,
                message_store,
                session_store,
                data_dir,
            )
            .await
        }

        async fn new_with_provider_and_session_store<F>(
            provider: Box<dyn kraai_provider_core::Provider>,
            configure_session_store: F,
        ) -> Option<Self>
        where
            F: FnOnce(Arc<dyn SessionStore>) -> Arc<dyn SessionStore>,
        {
            let mut providers = ProviderManager::new();
            providers.register_provider(ProviderId::new("mock"), provider);

            let mut tools = ToolManager::new();
            tools.register_tool(ApprovalTool);
            tools.register_tool(AutonomousTool);
            tools.register_tool(NoopTool);

            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let pid = std::process::id();
            let data_dir = std::env::temp_dir().join(format!("kraai-runtime-test-{pid}-{nanos}"));
            tokio::fs::create_dir_all(&data_dir)
                .await
                .expect("create temp runtime dir");
            let workspace_dir = data_dir.join("workspace");
            tokio::fs::create_dir_all(&workspace_dir)
                .await
                .expect("create temp workspace dir");
            let profile_dir = workspace_dir.join(".kraai");
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

            let message_store: Arc<dyn MessageStore> = Arc::new(FileMessageStore::new(&data_dir));
            let base_session_store: Arc<dyn SessionStore> =
                Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));
            let session_store = configure_session_store(base_session_store);

            Self::new_with_stores_and_parts(
                providers,
                tools,
                message_store,
                session_store,
                data_dir,
            )
            .await
        }

        async fn shutdown(self) {
            drop(self.handle);
            self.event_task.abort();
            self.runtime_task.abort();
            let _ = self.event_task.await;
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

    #[tokio::test]
    async fn provider_retry_observer_is_forwarded_to_runtime_events() -> Result<()> {
        let mut providers = ProviderManager::new();
        providers.register_provider(
            ProviderId::new("retry-mock"),
            Box::new(RetryNotifyingProvider {
                id: ProviderId::new("retry-mock"),
            }),
        );

        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_parts(
            providers,
            ToolManager::new(),
        ));
        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;

        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("hello"),
                String::from("mock-model"),
                String::from("retry-mock"),
            )
            .await?;

        let events = harness
            .events
            .wait_for("provider retry event", |events| {
                events.iter().any(|event| {
                    matches!(event, Event::ProviderRetryScheduled { session_id: event_session, .. } if event_session == &session_id)
                })
            })
            .await;

        let retry_event = events.iter().find_map(|event| match event {
            Event::ProviderRetryScheduled {
                session_id: event_session,
                provider_id,
                model_id,
                operation,
                retry_number,
                delay_seconds,
                reason,
            } if event_session == &session_id => Some((
                provider_id.clone(),
                model_id.clone(),
                operation.clone(),
                *retry_number,
                *delay_seconds,
                reason.clone(),
            )),
            _ => None,
        });

        assert_eq!(
            retry_event,
            Some((
                String::from("retry-mock"),
                String::from("mock-model"),
                String::from("responses"),
                1,
                1,
                String::from("HTTP 429"),
            ))
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn runtime_broadcasts_events_to_multiple_subscribers() -> Result<()> {
        let harness =
            runtime_harness_or_skip!(RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain(
                "shared event stream"
            ),]]));
        let mut first = harness.handle.subscribe();
        let mut second = harness.handle.subscribe();

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("hello"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        async fn collect_events(
            receiver: &mut broadcast::Receiver<Event>,
            session_id: &str,
        ) -> Result<Vec<Event>> {
            let mut events = Vec::new();
            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        let is_complete = matches!(
                            &event,
                            Event::StreamComplete {
                                session_id: completed_session,
                                ..
                            } if completed_session == session_id
                        );
                        events.push(event);
                        if is_complete {
                            return Ok(events);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(eyre!("event stream closed before completion"));
                    }
                }
            }
        }

        let first_events = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            collect_events(&mut first, &session_id),
        )
        .await
        .map_err(|_| eyre!("timed out waiting for first subscriber events"))??;
        let second_events = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            collect_events(&mut second, &session_id),
        )
        .await
        .map_err(|_| eyre!("timed out waiting for second subscriber events"))??;

        for events in [&first_events, &second_events] {
            assert!(events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamStart {
                        session_id: started_session,
                        ..
                    } if started_session == &session_id
                )
            }));
            assert!(events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamChunk {
                        session_id: chunk_session,
                        chunk,
                        ..
                    } if chunk_session == &session_id && chunk == "shared event stream"
                )
            }));
            assert!(events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamComplete {
                        session_id: completed_session,
                        ..
                    } if completed_session == &session_id
                )
            }));
        }

        harness.shutdown().await;
        Ok(())
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

    fn stream_start_count(events: &[Event], session_id: &str) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    Event::StreamStart {
                        session_id: event_session,
                        ..
                    } if event_session == session_id
                )
            })
            .count()
    }

    fn stream_complete_count(events: &[Event], session_id: &str) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    Event::StreamComplete {
                        session_id: event_session,
                        ..
                    } if event_session == session_id
                )
            })
            .count()
    }

    fn continuation_failed_count(events: &[Event], session_id: &str) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    Event::ContinuationFailed {
                        session_id: event_session,
                        ..
                    } if event_session == session_id
                )
            })
            .count()
    }

    fn call_id_for_queue_order(
        events: &[Event],
        session_id: &str,
        tool_id: &str,
        queue_order: u64,
    ) -> String {
        events
            .iter()
            .find_map(|event| match event {
                Event::ToolCallDetected {
                    session_id: event_session,
                    call_id,
                    tool_id: event_tool_id,
                    queue_order: event_queue_order,
                    ..
                } if event_session == session_id
                    && event_tool_id == tool_id
                    && *event_queue_order == queue_order =>
                {
                    Some(call_id.clone())
                }
                _ => None,
            })
            .expect("tool call id should exist")
    }

    #[test]
    fn tool_call_stream_guard_stops_on_trailing_non_whitespace() {
        let mut guard = ToolCallStreamGuard::default();

        let result = guard.ingest_chunk(
            "before\n<tool_call>\ntool: auto_tool\nvalue: alpha\n</tool_call>\nHallucinated",
        );

        assert!(result.should_stop);
        assert_eq!(
            result.accepted,
            "before\n<tool_call>\ntool: auto_tool\nvalue: alpha\n</tool_call>\n"
        );
        assert!(guard.finish().is_empty());
    }

    #[test]
    fn tool_call_stream_guard_allows_adjacent_tool_calls_across_chunks() {
        let mut guard = ToolCallStreamGuard::default();

        let first = guard
            .ingest_chunk("before\n<tool_call>\ntool: auto_tool\nvalue: alpha\n</tool_call>\n<to");
        assert!(!first.should_stop);
        assert_eq!(
            first.accepted,
            "before\n<tool_call>\ntool: auto_tool\nvalue: alpha\n</tool_call>\n"
        );

        let second = guard.ingest_chunk("ol_call>\ntool: auto_tool\nvalue: beta\n</tool_call>\n");
        assert!(!second.should_stop);
        assert_eq!(
            second.accepted,
            "<tool_call>\ntool: auto_tool\nvalue: beta\n</tool_call>\n"
        );
        assert!(guard.finish().is_empty());
    }

    #[test]
    fn tool_call_stream_guard_ignores_hidden_tool_calls_inside_prefix_think_blocks() {
        let mut guard = ToolCallStreamGuard::default();

        let result = guard.ingest_chunk(
            "<thinking class=\"chain\">\n\
<tool_call>\n\
tool: hidden_tool\n\
</tool_call>\n\
</thinking>\n\
before\n\
<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\n\
after",
        );

        assert!(result.should_stop);
        assert_eq!(
            result.accepted,
            "<thinking class=\"chain\">\n\
<tool_call>\n\
tool: hidden_tool\n\
</tool_call>\n\
</thinking>\n\
before\n\
<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\n"
        );
    }

    #[test]
    fn tool_call_stream_guard_drops_incomplete_next_tool_call_at_finish() {
        let mut guard = ToolCallStreamGuard::default();

        let result =
            guard.ingest_chunk("<tool_call>\ntool: auto_tool\nvalue: alpha\n</tool_call>\n<too");

        assert!(!result.should_stop);
        assert_eq!(
            result.accepted,
            "<tool_call>\ntool: auto_tool\nvalue: alpha\n</tool_call>\n"
        );
        assert!(guard.finish().is_empty());
    }

    #[tokio::test]
    async fn background_session_stream_continues_after_switch() -> Result<()> {
        let gate = Arc::new(tokio::sync::Notify::new());
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
            vec![
                ScriptedChunk::plain("session-a chunk 1 "),
                ScriptedChunk::gated("session-a chunk 2", gate.clone()),
            ],
            vec![ScriptedChunk::plain("session-b complete")],
        ]));

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
    async fn completion_save_failure_rolls_back_stream_and_allows_retry() -> Result<()> {
        let fail_completion_save = Arc::new(AtomicBool::new(true));
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_message_store(
            vec![
                vec![ScriptedChunk::plain("first reply")],
                vec![ScriptedChunk::plain("second reply")],
            ],
            {
                let fail_completion_save = fail_completion_save.clone();
                move |base_store| {
                    Arc::new(FailOnAssistantCompletionMessageStore {
                        inner: base_store,
                        should_fail: fail_completion_save,
                    })
                }
            },
        ));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("first prompt"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let events = harness
            .events
            .wait_for("stream completion persistence failure", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::StreamError {
                            session_id: event_session,
                            error,
                            ..
                        } if event_session == &session_id
                            && error.contains("intentional assistant completion save failure")
                    )
                })
            })
            .await;

        assert_eq!(stream_complete_count(&events, &session_id), 0);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                Event::HistoryUpdated { session_id: event_session }
                    if event_session == &session_id
            )
        }));

        let failed_history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert_eq!(failed_history.len(), 1);
        assert!(
            failed_history
                .values()
                .all(|message| message.content != "first reply")
        );

        fail_completion_save.store(false, Ordering::SeqCst);
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("second prompt"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("retry completion after rollback", |events| {
                stream_complete_count(events, &session_id) == 1
            })
            .await;

        let recovered_history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(
            recovered_history
                .values()
                .any(|message| message.content == "second reply")
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn start_failure_surfaces_rollback_error_without_clearing_active_turn() -> Result<()> {
        let provider_started = Arc::new(tokio::sync::Notify::new());
        let provider_release = Arc::new(tokio::sync::Notify::new());
        let fail_session_save = Arc::new(AtomicBool::new(false));
        let harness =
            runtime_harness_or_skip!(RuntimeTestHarness::new_with_provider_and_session_store(
                Box::new(DeferredFailingProvider {
                    id: ProviderId::new("mock"),
                    started: provider_started.clone(),
                    release: provider_release.clone(),
                    failure_message: String::from("provider start failed"),
                }),
                {
                    let fail_session_save = fail_session_save.clone();
                    move |base_store| {
                        Arc::new(FailOnDemandSessionStore {
                            inner: base_store,
                            should_fail: fail_session_save,
                        })
                    }
                },
            ));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("trigger failure"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        provider_started.notified().await;
        fail_session_save.store(true, Ordering::SeqCst);
        provider_release.notify_one();

        let events = harness
            .events
            .wait_for("rollback failure surfaced", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::Error(message)
                            if message.contains("Failed to roll back stream")
                                && message.contains("intentional session save failure")
                    )
                })
            })
            .await;

        assert!(events.iter().any(|event| {
            matches!(
                event,
                Event::Error(message) if message == "provider start failed"
            )
        }));
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                Event::HistoryUpdated { session_id: event_session }
                    if event_session == &session_id
            )
        }));

        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("retry should stay blocked"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        tokio::time::sleep(Duration::from_millis(50)).await;
        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(
            history
                .values()
                .all(|message| message.content != "retry should stay blocked")
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn background_session_tool_approval_and_continuation_work_after_switch() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
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
        ]));

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
    async fn continuation_waits_for_all_tools_from_one_message_across_split_executions()
    -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
            vec![ScriptedChunk::plain(
                "<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
<tool_call>\n\
tool: mock_tool\n\
value: beta\n\
</tool_call>",
            )],
            vec![ScriptedChunk::plain("continuation complete")],
        ]));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("run two tools"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let detection_events = harness
            .events
            .wait_for("two tool detections", |events| {
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
                    == 2
            })
            .await;

        let first_call_id = call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 0);
        let second_call_id =
            call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 1);

        harness
            .handle
            .approve_tool(session_id.clone(), first_call_id.clone())
            .await?;
        harness
            .handle
            .execute_approved_tools(session_id.clone())
            .await?;

        harness
            .events
            .wait_for("first tool result without continuation", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            call_id,
                            tool_id,
                            denied,
                            ..
                        } if event_session == &session_id
                            && call_id == &first_call_id
                            && tool_id == "mock_tool"
                            && !denied
                    )
                })
            })
            .await;

        tokio::time::sleep(Duration::from_millis(100)).await;
        let events_after_first = harness.events.snapshot();
        assert_eq!(stream_start_count(&events_after_first, &session_id), 1);
        assert_eq!(stream_complete_count(&events_after_first, &session_id), 1);

        harness
            .handle
            .approve_tool(session_id.clone(), second_call_id.clone())
            .await?;
        harness
            .handle
            .execute_approved_tools(session_id.clone())
            .await?;

        harness
            .events
            .wait_for("second tool result and single continuation", |events| {
                let second_result_ready = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            call_id,
                            tool_id,
                            denied,
                            ..
                        } if event_session == &session_id
                            && call_id == &second_call_id
                            && tool_id == "mock_tool"
                            && !denied
                    )
                });
                second_result_ready && stream_complete_count(events, &session_id) == 2
            })
            .await;

        let final_events = harness.events.snapshot();
        assert_eq!(stream_start_count(&final_events, &session_id), 2);
        assert_eq!(stream_complete_count(&final_events, &session_id), 2);
        assert_eq!(continuation_failed_count(&final_events, &session_id), 0);

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(
            history
                .values()
                .any(|message| message.content == "continuation complete")
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn continue_session_starts_new_assistant_turn_without_new_user_message() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
            vec![ScriptedChunk::plain("first reply")],
            vec![ScriptedChunk::plain("second reply")],
        ]));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("hello"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("first turn completion", |events| {
                stream_complete_count(events, &session_id) == 1
            })
            .await;

        harness.handle.continue_session(session_id.clone()).await?;

        harness
            .events
            .wait_for("continued turn completion", |events| {
                stream_complete_count(events, &session_id) == 2
            })
            .await;

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        let user_count = history
            .values()
            .filter(|message| message.role == kraai_types::ChatRole::User)
            .count();
        assert_eq!(user_count, 1);
        assert!(
            history
                .values()
                .any(|message| message.content == "first reply")
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
    async fn overlapping_execute_requests_wait_for_in_flight_tools_from_the_same_message()
    -> Result<()> {
        let started = Arc::new(tokio::sync::Notify::new());
        let ready = Arc::new(tokio::sync::Barrier::new(2));
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_tools(
            vec![
                vec![ScriptedChunk::plain(
                    "<tool_call>\n\
tool: batch_blocking_tool\n\
value: alpha\n\
</tool_call>\n\
<tool_call>\n\
tool: batch_blocking_tool\n\
value: beta\n\
</tool_call>",
                )],
                vec![ScriptedChunk::plain("continuation complete")],
            ],
            {
                let started = started.clone();
                let ready = ready.clone();
                move |tools| {
                    tools.register_tool(BatchBlockingApprovalTool { started, ready });
                }
            },
        ));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("run overlapping executes"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let detection_events = harness
            .events
            .wait_for("two blocking tool detections", |events| {
                events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::ToolCallDetected {
                                session_id: event_session,
                                tool_id,
                                ..
                            } if event_session == &session_id && tool_id == "batch_blocking_tool"
                        )
                    })
                    .count()
                    == 2
            })
            .await;

        let first_call_id =
            call_id_for_queue_order(&detection_events, &session_id, "batch_blocking_tool", 0);
        let second_call_id =
            call_id_for_queue_order(&detection_events, &session_id, "batch_blocking_tool", 1);

        harness
            .handle
            .approve_tool(session_id.clone(), first_call_id.clone())
            .await?;
        harness
            .handle
            .execute_approved_tools(session_id.clone())
            .await?;
        started.notified().await;
        harness
            .handle
            .approve_tool(session_id.clone(), second_call_id.clone())
            .await?;
        harness
            .handle
            .execute_approved_tools(session_id.clone())
            .await?;

        harness
            .events
            .wait_for("first overlapping tool result", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            call_id,
                            tool_id,
                            denied,
                            ..
                        } if event_session == &session_id
                            && call_id == &first_call_id
                            && tool_id == "batch_blocking_tool"
                            && !denied
                    )
                })
            })
            .await;

        tokio::time::sleep(Duration::from_millis(100)).await;
        let events_after_first = harness.events.snapshot();
        assert_eq!(stream_start_count(&events_after_first, &session_id), 1);
        assert_eq!(stream_complete_count(&events_after_first, &session_id), 1);

        harness
            .events
            .wait_for("both blocking results and one continuation", |events| {
                let result_count = events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::ToolResultReady {
                                session_id: event_session,
                                tool_id,
                                denied,
                                ..
                            } if event_session == &session_id
                                && tool_id == "batch_blocking_tool"
                                && !denied
                        )
                    })
                    .count();
                result_count == 2 && stream_complete_count(events, &session_id) == 2
            })
            .await;

        let final_events = harness.events.snapshot();
        assert_eq!(stream_start_count(&final_events, &session_id), 2);
        assert_eq!(stream_complete_count(&final_events, &session_id), 2);
        assert_eq!(continuation_failed_count(&final_events, &session_id), 0);

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn auto_approved_and_manual_tools_share_one_continuation_boundary() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
            vec![ScriptedChunk::plain(
                "<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\n\
<tool_call>\n\
tool: mock_tool\n\
value: beta\n\
</tool_call>",
            )],
            vec![ScriptedChunk::plain("continuation complete")],
        ]));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("run mixed tools"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let events = harness
            .events
            .wait_for("auto result plus manual detection", |events| {
                let auto_result_ready = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            tool_id,
                            success,
                            denied,
                            ..
                        } if event_session == &session_id
                            && tool_id == "auto_tool"
                            && *success
                            && !denied
                    )
                });
                let manual_detected = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolCallDetected {
                            session_id: event_session,
                            tool_id,
                            ..
                        } if event_session == &session_id && tool_id == "mock_tool"
                    )
                });
                auto_result_ready && manual_detected
            })
            .await;

        let manual_call_id = call_id_for_queue_order(&events, &session_id, "mock_tool", 1);

        tokio::time::sleep(Duration::from_millis(100)).await;
        let events_after_auto = harness.events.snapshot();
        assert_eq!(stream_start_count(&events_after_auto, &session_id), 1);
        assert_eq!(stream_complete_count(&events_after_auto, &session_id), 1);

        harness
            .handle
            .approve_tool(session_id.clone(), manual_call_id.clone())
            .await?;
        harness
            .handle
            .execute_approved_tools(session_id.clone())
            .await?;

        harness
            .events
            .wait_for("manual result and continuation", |events| {
                let manual_result_ready = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            call_id,
                            tool_id,
                            denied,
                            ..
                        } if event_session == &session_id
                            && call_id == &manual_call_id
                            && tool_id == "mock_tool"
                            && !denied
                    )
                });
                manual_result_ready && stream_complete_count(events, &session_id) == 2
            })
            .await;

        let final_events = harness.events.snapshot();
        assert_eq!(stream_start_count(&final_events, &session_id), 2);
        assert_eq!(stream_complete_count(&final_events, &session_id), 2);

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn denied_and_approved_tools_finish_before_single_continuation_starts() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
            vec![ScriptedChunk::plain(
                "<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
<tool_call>\n\
tool: mock_tool\n\
value: beta\n\
</tool_call>",
            )],
            vec![ScriptedChunk::plain("continuation complete")],
        ]));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("run approve and deny"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let detection_events = harness
            .events
            .wait_for("two tool detections for mixed decision batch", |events| {
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
                    == 2
            })
            .await;

        let denied_call_id =
            call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 0);
        let approved_call_id =
            call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 1);

        harness
            .handle
            .deny_tool(session_id.clone(), denied_call_id.clone())
            .await?;
        harness
            .handle
            .approve_tool(session_id.clone(), approved_call_id.clone())
            .await?;
        harness
            .handle
            .execute_approved_tools(session_id.clone())
            .await?;

        harness
            .events
            .wait_for("mixed decision tool results and continuation", |events| {
                let denied_result = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            call_id,
                            tool_id,
                            denied,
                            ..
                        } if event_session == &session_id
                            && call_id == &denied_call_id
                            && tool_id == "mock_tool"
                            && *denied
                    )
                });
                let approved_result = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            call_id,
                            tool_id,
                            denied,
                            ..
                        } if event_session == &session_id
                            && call_id == &approved_call_id
                            && tool_id == "mock_tool"
                            && !denied
                    )
                });
                denied_result && approved_result && stream_complete_count(events, &session_id) == 2
            })
            .await;

        let final_events = harness.events.snapshot();
        let continuation_start_index = final_events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| match event {
                Event::StreamStart {
                    session_id: event_session,
                    ..
                } if event_session == &session_id => Some(index),
                _ => None,
            })
            .nth(1)
            .expect("continuation stream should start once");
        let denied_result_index = final_events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        call_id,
                        denied,
                        ..
                    } if event_session == &session_id
                        && call_id == &denied_call_id
                        && *denied
                )
            })
            .expect("denied tool result should exist");
        let approved_result_index = final_events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        call_id,
                        denied,
                        ..
                    } if event_session == &session_id
                        && call_id == &approved_call_id
                        && !denied
                )
            })
            .expect("approved tool result should exist");

        assert!(denied_result_index < continuation_start_index);
        assert!(approved_result_index < continuation_start_index);
        assert_eq!(stream_start_count(&final_events, &session_id), 2);
        assert_eq!(stream_complete_count(&final_events, &session_id), 2);

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn auto_approve_option_bypasses_manual_tool_confirmation() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
            vec![ScriptedChunk::plain(
                "<tool_call>\n\
tool: mock_tool\n\
value: beta\n\
</tool_call>",
            )],
            vec![ScriptedChunk::plain("continuation complete")],
        ]));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message_with_options(
                session_id.clone(),
                String::from("run manual tool without confirmation"),
                String::from("mock-model"),
                String::from("mock"),
                true,
            )
            .await?;

        harness
            .events
            .wait_for("manual tool auto-approved by option", |events| {
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
                            && tool_id == "mock_tool"
                            && *success
                            && !denied
                    )
                });
                tool_result_ready && stream_complete_count(events, &session_id) == 2
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
                } if event_session == &session_id && tool_id == "mock_tool"
            )
        }));

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn multiple_tools_executed_together_start_only_one_continuation() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
            vec![ScriptedChunk::plain(
                "<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
<tool_call>\n\
tool: mock_tool\n\
value: beta\n\
</tool_call>",
            )],
            vec![ScriptedChunk::plain("continuation complete")],
        ]));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("approve all tools"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let detection_events = harness
            .events
            .wait_for("two tool detections for single execution", |events| {
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
                    == 2
            })
            .await;

        let first_call_id = call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 0);
        let second_call_id =
            call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 1);

        harness
            .handle
            .approve_tool(session_id.clone(), first_call_id.clone())
            .await?;
        harness
            .handle
            .approve_tool(session_id.clone(), second_call_id.clone())
            .await?;
        harness
            .handle
            .execute_approved_tools(session_id.clone())
            .await?;

        harness
            .events
            .wait_for("single continuation after one execution batch", |events| {
                let first_result = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            call_id,
                            ..
                        } if event_session == &session_id && call_id == &first_call_id
                    )
                });
                let second_result = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            call_id,
                            ..
                        } if event_session == &session_id && call_id == &second_call_id
                    )
                });
                first_result && second_result && stream_complete_count(events, &session_id) == 2
            })
            .await;

        let final_events = harness.events.snapshot();
        assert_eq!(stream_start_count(&final_events, &session_id), 2);
        assert_eq!(stream_complete_count(&final_events, &session_id), 2);

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn continuation_failure_still_happens_once_after_all_results_in_a_tool_batch()
    -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_tools(
            vec![vec![ScriptedChunk::plain(
                "<tool_call>\n\
tool: failing_tool\n\
value: alpha\n\
</tool_call>\n\
<tool_call>\n\
tool: failing_tool\n\
value: beta\n\
</tool_call>",
            )]],
            |tools| {
                tools.register_tool(FailingApprovalTool);
            },
        ));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("run failing tools"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let detection_events = harness
            .events
            .wait_for("two failing tool detections", |events| {
                events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::ToolCallDetected {
                                session_id: event_session,
                                tool_id,
                                ..
                            } if event_session == &session_id && tool_id == "failing_tool"
                        )
                    })
                    .count()
                    == 2
            })
            .await;

        let first_call_id =
            call_id_for_queue_order(&detection_events, &session_id, "failing_tool", 0);
        let second_call_id =
            call_id_for_queue_order(&detection_events, &session_id, "failing_tool", 1);

        harness
            .handle
            .approve_tool(session_id.clone(), first_call_id.clone())
            .await?;
        harness
            .handle
            .approve_tool(session_id.clone(), second_call_id.clone())
            .await?;
        harness
            .handle
            .execute_approved_tools(session_id.clone())
            .await?;

        harness
            .events
            .wait_for(
                "tool failures followed by one continuation failure",
                |events| {
                    let result_count = events
                        .iter()
                        .filter(|event| {
                            matches!(
                                event,
                                Event::ToolResultReady {
                                    session_id: event_session,
                                    tool_id,
                                    success,
                                    denied,
                                    ..
                                } if event_session == &session_id
                                    && tool_id == "failing_tool"
                                    && !success
                                    && !denied
                            )
                        })
                        .count();
                    result_count == 2 && continuation_failed_count(events, &session_id) == 1
                },
            )
            .await;

        let final_events = harness.events.snapshot();
        let continuation_failed_index = final_events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    Event::ContinuationFailed {
                        session_id: event_session,
                        ..
                    } if event_session == &session_id
                )
            })
            .expect("continuation failure should exist");
        let last_result_index = final_events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| match event {
                Event::ToolResultReady {
                    session_id: event_session,
                    tool_id,
                    success,
                    denied,
                    ..
                } if event_session == &session_id
                    && tool_id == "failing_tool"
                    && !success
                    && !denied =>
                {
                    Some(index)
                }
                _ => None,
            })
            .next_back()
            .expect("failing tool results should exist");

        assert!(last_result_index < continuation_failed_index);
        assert_eq!(continuation_failed_count(&final_events, &session_id), 1);

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        let failed_result_count = history
            .values()
            .filter(|message| {
                message
                    .content
                    .contains("Tool 'failing_tool' result:\n{\n  \"error\": \"tool exploded\"\n}")
            })
            .count();
        assert_eq!(failed_result_count, 2);

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn session_operations_remain_responsive_while_tool_executes() -> Result<()> {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_tools(
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
        ));

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
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_tools(
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
        ));

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
    async fn trailing_visible_text_after_tool_call_is_truncated_and_continues() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
            vec![ScriptedChunk::plain(
                "before tool\n\
<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\n\
hallucinated tool result",
            )],
            vec![ScriptedChunk::plain("continuation complete")],
        ]));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("trigger truncation"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let events = harness
            .events
            .wait_for("continuation after truncated suffix", |events| {
                stream_complete_count(events, &session_id) >= 2
                    && events.iter().any(|event| {
                        matches!(
                            event,
                            Event::ToolResultReady {
                                session_id: event_session,
                                tool_id,
                                ..
                            } if event_session == &session_id && tool_id == "auto_tool"
                        )
                    })
            })
            .await;

        assert!(!events.iter().any(|event| {
            matches!(
                event,
                Event::StreamCancelled {
                    session_id: event_session,
                    ..
                } if event_session == &session_id
            )
        }));
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                Event::StreamError {
                    session_id: event_session,
                    ..
                } if event_session == &session_id
            )
        }));

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        let first_assistant = history
            .values()
            .find(|message| {
                message.role == ChatRole::Assistant && message.content.contains("tool: auto_tool")
            })
            .expect("assistant tool-call message should exist");
        assert_eq!(
            first_assistant.content,
            "before tool\n\
<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\n"
        );
        assert!(
            !history
                .values()
                .any(|message| { message.content.contains("hallucinated tool result") })
        );
        assert!(
            history
                .values()
                .any(|message| message.content == "continuation complete")
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn split_adjacent_tool_calls_are_both_detected() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![vec![
            ScriptedChunk::plain(
                "before\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
<to",
            ),
            ScriptedChunk::plain(
                "ol_call>\n\
tool: mock_tool\n\
value: beta\n\
</tool_call>\n",
            ),
        ]]));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("two tools"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let events = harness
            .events
            .wait_for("two tool detections", |events| {
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
                    == 2
            })
            .await;

        assert_eq!(
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
                .count(),
            2
        );

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        let assistant = history
            .values()
            .find(|message| {
                message.role == ChatRole::Assistant && message.content.contains("value: alpha")
            })
            .expect("assistant message should persist both tool calls");
        assert!(assistant.content.contains("value: alpha"));
        assert!(assistant.content.contains("value: beta"));

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn visible_text_between_tool_calls_truncates_at_first_completed_tool() -> Result<()> {
        let harness =
            runtime_harness_or_skip!(RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain(
                "before\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
not allowed\n\
<tool_call>\n\
tool: mock_tool\n\
value: beta\n\
</tool_call>",
            ),]]));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("truncate between tools"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("single tool detection after truncation", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolCallDetected {
                            session_id: event_session,
                            tool_id,
                            ..
                        } if event_session == &session_id && tool_id == "mock_tool"
                    )
                })
            })
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let events = harness.events.snapshot();
        assert_eq!(
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
                .count(),
            1
        );

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        let assistant = history
            .values()
            .find(|message| {
                message.role == ChatRole::Assistant && message.content.contains("value: alpha")
            })
            .expect("assistant message should persist first tool only");
        assert_eq!(
            assistant.content,
            "before\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n"
        );
        assert!(!assistant.content.contains("value: beta"));
        assert!(!assistant.content.contains("not allowed"));

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn trailing_visible_text_split_across_chunks_still_truncates_cleanly() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
            vec![
                ScriptedChunk::plain(
                    "before\n\
<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\nha",
                ),
                ScriptedChunk::plain("llucinated continuation"),
            ],
            vec![ScriptedChunk::plain("continuation complete")],
        ]));

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("split trailing text"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        let events = harness
            .events
            .wait_for("split truncation continuation", |events| {
                stream_complete_count(events, &session_id) >= 2
            })
            .await;

        assert!(!events.iter().any(|event| {
            matches!(
                event,
                Event::StreamCancelled {
                    session_id: event_session,
                    ..
                } if event_session == &session_id
            )
        }));
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                Event::StreamError {
                    session_id: event_session,
                    ..
                } if event_session == &session_id
            )
        }));

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        let assistant = history
            .values()
            .find(|message| {
                message.role == ChatRole::Assistant && message.content.contains("tool: auto_tool")
            })
            .expect("assistant tool-call message should exist");
        assert_eq!(
            assistant.content,
            "before\n\
<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\n"
        );
        assert!(
            !history
                .values()
                .any(|message| { message.content.contains("hallucinated continuation") })
        );

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

        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_parts(
            providers,
            ToolManager::new(),
        ));
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
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![vec![
            ScriptedChunk::plain("partial "),
            ScriptedChunk::gated("more text", gate),
        ]]));

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

        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_parts(
            providers,
            ToolManager::new(),
        ));
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
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![vec![
            ScriptedChunk::plain("before tool\n"),
            ScriptedChunk::gated(
                "<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>",
                gate,
            ),
        ]]));

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
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
            vec![
                ScriptedChunk::plain("partial "),
                ScriptedChunk::gated("blocked", gate),
            ],
            vec![ScriptedChunk::plain("second reply")],
        ]));

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
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![vec![
            ScriptedChunk::plain("partial "),
            ScriptedChunk::gated("blocked", gate),
        ]]));

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
    async fn queued_messages_wait_for_tool_batch_completion_before_draining() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
            vec![ScriptedChunk::plain(
                "before tool\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>",
            )],
            vec![ScriptedChunk::plain("continuation complete")],
            vec![ScriptedChunk::plain("queued second reply")],
            vec![ScriptedChunk::plain("queued third reply")],
        ]));

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

        let detection_events = harness
            .events
            .wait_for("tool detection", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolCallDetected {
                            session_id: event_session,
                            tool_id,
                            ..
                        } if event_session == &session_id && tool_id == "mock_tool"
                    )
                })
            })
            .await;

        let first_call_id = call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 0);

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
            .handle
            .send_message(
                session_id.clone(),
                String::from("third message"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        tokio::time::sleep(Duration::from_millis(100)).await;
        let events_before_tools = harness.events.snapshot();
        assert_eq!(stream_start_count(&events_before_tools, &session_id), 1);

        harness
            .handle
            .approve_tool(session_id.clone(), first_call_id)
            .await?;
        harness
            .handle
            .execute_approved_tools(session_id.clone())
            .await?;

        harness
            .events
            .wait_for("drain queued messages", |events| {
                stream_complete_count(events, &session_id) >= 4
            })
            .await;

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(
            history
                .values()
                .any(|message| message.content == "continuation complete")
        );
        assert!(
            history
                .values()
                .any(|message| message.content == "queued second reply")
        );
        assert!(
            history
                .values()
                .any(|message| message.content == "queued third reply")
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn repeated_malformed_tool_calls_continue_without_deadlocking() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
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
        ]));

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
        let harness =
            runtime_harness_or_skip!(RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain(
                "<think>\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
</think>",
            )]]));

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
        let harness =
            runtime_harness_or_skip!(RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain(
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
            )]]));

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
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new(vec![
            vec![ScriptedChunk::plain(
                "<think>\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>",
            )],
            vec![ScriptedChunk::plain("continuation complete")],
        ]));

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
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_tools(
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
        ));

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
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_tools(
            vec![
                vec![ScriptedChunk::plain(
                    r#"<tool_call>
tool: read_files
files[1]: src/lib.rs
</tool_call>"#,
                )],
                vec![ScriptedChunk::plain("first continuation complete")],
                vec![ScriptedChunk::plain(
                    r#"<tool_call>
tool: edit_file
path: src/lib.rs
create: false
edits[1]{start_line,end_line,old_text,new_text}:
  1,1,old,new
</tool_call>"#,
                )],
                vec![ScriptedChunk::plain("second continuation complete")],
            ],
            |tools| {
                tools.register_tool(ReadFileTool);
                tools.register_tool(EditFileTool);
            },
        ));

        let workspace_src = harness.data_dir.join("workspace").join("src");
        tokio::fs::create_dir_all(&workspace_src).await?;
        tokio::fs::write(workspace_src.join("lib.rs"), "old").await?;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("read file"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("read_files continuation", |events| {
                events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::StreamComplete { session_id: event_session, .. }
                                if event_session == &session_id
                        )
                    })
                    .count()
                    >= 2
            })
            .await;

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
                tool_result_ready && stream_completions >= 4
            })
            .await;

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(
            history
                .values()
                .any(|message| message.content == "second continuation complete")
        );
        assert_eq!(
            tokio::fs::read_to_string(workspace_src.join("lib.rs")).await?,
            "new"
        );

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn batched_read_files_does_not_unlock_same_turn_edit_file() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_tools(
            vec![
                vec![ScriptedChunk::plain(
                    r#"<tool_call>
tool: read_files
files[1]: src/lib.rs
</tool_call>

<tool_call>
tool: edit_file
path: src/lib.rs
create: false
edits[1]{start_line,end_line,old_text,new_text}:
  1,1,old,new
</tool_call>"#,
                )],
                vec![ScriptedChunk::plain("continuation complete")],
            ],
            |tools| {
                tools.register_tool(ReadFileTool);
                tools.register_tool(EditFileTool);
            },
        ));

        let workspace_src = harness.data_dir.join("workspace").join("src");
        tokio::fs::create_dir_all(&workspace_src).await?;
        let file_path = workspace_src.join("lib.rs");
        tokio::fs::write(&file_path, "old").await?;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("read then edit"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("batched read/edit tool results", |events| {
                let read_ready = events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            tool_id,
                            success,
                            denied,
                            ..
                        } if event_session == &session_id
                            && tool_id == "read_files"
                            && *success
                            && !denied
                    )
                });
                let edit_failed = events.iter().any(|event| {
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
                            && !*success
                            && !denied
                    )
                });
                read_ready && edit_failed
            })
            .await;

        let history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(history.values().any(|message| {
            message
                .content
                .contains("edit_file requires the current file contents to be read first")
        }));
        assert_eq!(tokio::fs::read_to_string(&file_path).await?, "old");

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn open_file_refresh_allows_next_turn_edit_without_explicit_reread() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_tools(
            vec![
                vec![ScriptedChunk::plain(
                    r#"<tool_call>
tool: open_file
path: src/lib.rs
</tool_call>"#,
                )],
                vec![ScriptedChunk::plain("first continuation complete")],
                vec![ScriptedChunk::plain(
                    r#"<tool_call>
tool: edit_file
path: src/lib.rs
create: false
edits[1]{start_line,end_line,old_text,new_text}:
  1,1,updated,rewritten
</tool_call>"#,
                )],
                vec![ScriptedChunk::plain("second continuation complete")],
            ],
            |tools| {
                tools.register_tool(OpenFileTool);
                tools.register_tool(EditFileTool);
            },
        ));

        let workspace_src = harness.data_dir.join("workspace").join("src");
        tokio::fs::create_dir_all(&workspace_src).await?;
        let file_path = workspace_src.join("lib.rs");
        tokio::fs::write(&file_path, "initial").await?;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("open the file"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("open_file continuation", |events| {
                events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::StreamComplete { session_id: event_session, .. }
                                if event_session == &session_id
                        )
                    })
                    .count()
                    >= 2
            })
            .await;

        tokio::fs::write(&file_path, "updated").await?;

        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("edit the open file"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("second turn edit succeeds", |events| {
                let edit_succeeded = events.iter().any(|event| {
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
                            Event::StreamComplete { session_id: event_session, .. }
                                if event_session == &session_id
                        )
                    })
                    .count();
                edit_succeeded && stream_completions >= 4
            })
            .await;

        assert_eq!(tokio::fs::read_to_string(&file_path).await?, "rewritten");

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn second_same_turn_edit_fails_after_first_changes_file() -> Result<()> {
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_tools(
            vec![
                vec![ScriptedChunk::plain(
                    r#"<tool_call>
tool: open_file
path: src/lib.rs
</tool_call>"#,
                )],
                vec![ScriptedChunk::plain("first continuation complete")],
                vec![ScriptedChunk::plain(
                    r#"<tool_call>
tool: edit_file
path: src/lib.rs
create: false
edits[1]{start_line,end_line,old_text,new_text}:
  1,1,old,new
</tool_call>

<tool_call>
tool: edit_file
path: src/lib.rs
create: false
edits[1]{start_line,end_line,old_text,new_text}:
  1,1,new,newer
</tool_call>"#,
                )],
                vec![ScriptedChunk::plain("second continuation complete")],
            ],
            |tools| {
                tools.register_tool(OpenFileTool);
                tools.register_tool(EditFileTool);
            },
        ));

        let workspace_src = harness.data_dir.join("workspace").join("src");
        tokio::fs::create_dir_all(&workspace_src).await?;
        let file_path = workspace_src.join("lib.rs");
        tokio::fs::write(&file_path, "old").await?;

        let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("open the file"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("open_file before double edit", |events| {
                events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::StreamComplete { session_id: event_session, .. }
                                if event_session == &session_id
                        )
                    })
                    .count()
                    >= 2
            })
            .await;

        harness
            .handle
            .send_message(
                session_id.clone(),
                String::from("double edit"),
                String::from("mock-model"),
                String::from("mock"),
            )
            .await?;

        harness
            .events
            .wait_for("double edit results", |events| {
                let successes = events
                    .iter()
                    .filter(|event| {
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
                    .count();
                let failures = events
                    .iter()
                    .filter(|event| {
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
                                && !*success
                                && !denied
                        )
                    })
                    .count();
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
                successes >= 1 && failures >= 1 && stream_completions >= 4
            })
            .await;

        assert_eq!(tokio::fs::read_to_string(&file_path).await?, "new");

        harness.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn parse_failure_history_write_error_stops_continuation_and_recovers() -> Result<()> {
        let fail_tool_history_save = Arc::new(AtomicBool::new(true));
        let harness = runtime_harness_or_skip!(RuntimeTestHarness::new_with_message_store(
            vec![
                vec![ScriptedChunk::plain(
                    "<tool_call>\n\
tool: mock_tool\n\
value: {\n\
</tool_call>",
                )],
                vec![ScriptedChunk::plain("retry reply")],
            ],
            {
                let fail_tool_history_save = fail_tool_history_save.clone();
                move |base_store| {
                    Arc::new(FailOnToolMessageStore {
                        inner: base_store,
                        should_fail: fail_tool_history_save,
                    })
                }
            },
        ));

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

        let events = harness
            .events
            .wait_for("parse failure history persistence error", |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ContinuationFailed {
                            session_id: event_session,
                            error,
                        } if event_session == &session_id
                            && error.contains("intentional tool history save failure")
                    )
                })
            })
            .await;

        assert_eq!(stream_complete_count(&events, &session_id), 1);
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                Event::ToolCallDetected {
                    session_id: event_session,
                    ..
                } if event_session == &session_id
            )
        }));

        let failed_history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(
            failed_history
                .values()
                .all(|message| !message.content.contains("Failed to parse tool call"))
        );
        assert!(
            failed_history
                .values()
                .all(|message| message.content != "retry reply")
        );

        fail_tool_history_save.store(false, Ordering::SeqCst);
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
            .wait_for("retry after parse failure write error", |events| {
                events
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
                    .count()
                    >= 2
            })
            .await;

        let recovered_history = harness.handle.get_chat_history(session_id.clone()).await?;
        assert!(
            recovered_history
                .values()
                .any(|message| message.content == "retry reply")
        );

        harness.shutdown().await;
        Ok(())
    }
}
