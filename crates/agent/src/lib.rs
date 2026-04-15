#![forbid(unsafe_code)]

mod profiles;
mod tool_state;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use persistence::{MessageStore, SessionMeta, SessionStore};
use profiles::{AgentProfile, ResolvedProfiles, resolve_profiles};
use provider_core::{Model, ProviderManager, ProviderManagerConfig, ProviderRegistry};
use tokio::sync::RwLock;
use tool_core::{
    PreparedToolCall, ToolManager,
    toon_parser::{self, ParseFailure, ParseFailureKind},
};
use tool_state::{
    refresh_and_render_system_prompt as render_tool_state_prompt, resolve_snapshot_from_history,
};
use types::{
    AgentProfilesState, CallId, ChatMessage, ChatRole, Message, MessageId, MessageStatus, ModelId,
    ProviderId, RiskLevel, ToolCall, ToolCallAssessment, ToolId, ToolResult, ToolStateSnapshot,
};
use ulid::Ulid;

const DEFAULT_AGENT_PROFILE_ID: &str = "plan-code";
const SESSION_TITLE_MAX_CHARS: usize = 60;

fn title_from_user_prompt(prompt: &str) -> Option<String> {
    let normalized = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = normalized.chars().take(SESSION_TITLE_MAX_CHARS).collect();
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

#[derive(Clone)]
pub struct PendingToolCall {
    pub call: ToolCall,
    pub source_message_id: MessageId,
    pub prepared: PreparedToolCall,
    pub description: String,
    pub assessment: ToolCallAssessment,
    pub config: types::ToolCallGlobalConfig,
    pub tool_state_snapshot: ToolStateSnapshot,
    pub status: PermissionStatus,
    pub queue_order: u64,
}

impl std::fmt::Debug for PendingToolCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingToolCall")
            .field("call", &self.call)
            .field("source_message_id", &self.source_message_id)
            .field("description", &self.description)
            .field("assessment", &self.assessment)
            .field("config", &self.config)
            .field("tool_state_snapshot", &self.tool_state_snapshot)
            .field("status", &self.status)
            .field("queue_order", &self.queue_order)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PermissionStatus {
    Pending,
    Approved,
    Denied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingToolInfo {
    pub call_id: CallId,
    pub tool_id: ToolId,
    pub args: serde_json::Value,
    pub description: String,
    pub risk_level: RiskLevel,
    pub reasons: Vec<String>,
    pub approved: Option<bool>,
    pub queue_order: u64,
}

#[derive(Clone)]
pub enum ToolExecutionPayload {
    Approved {
        prepared: PreparedToolCall,
        config: types::ToolCallGlobalConfig,
        tool_state_snapshot: ToolStateSnapshot,
    },
    Denied,
}

#[derive(Clone)]
pub struct ToolExecutionRequest {
    pub call_id: CallId,
    pub tool_id: ToolId,
    pub source_message_id: MessageId,
    pub payload: ToolExecutionPayload,
}

#[derive(Clone, Debug)]
pub struct PendingStreamRequest {
    pub message_id: MessageId,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub provider_messages: Vec<ChatMessage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelledStreamResult {
    pub session_id: String,
    pub message_id: MessageId,
    pub persisted: bool,
}

#[derive(Clone)]
struct SessionRuntimeState {
    active_tool_config: types::ToolCallGlobalConfig,
    pending_tool_config: Option<types::ToolCallGlobalConfig>,
    pending_tool_calls: HashMap<CallId, PendingToolCall>,
    in_flight_tool_calls: HashMap<MessageId, usize>,
    next_tool_queue_order: u64,
    last_model: Option<ModelId>,
    last_provider: Option<ProviderId>,
    active_turn_profile: Option<AgentProfile>,
    active_turn_auto_approve: bool,
    active_turn_tool_state_snapshot: Option<ToolStateSnapshot>,
}

impl SessionRuntimeState {
    fn new(workspace_dir: PathBuf) -> Self {
        Self {
            active_tool_config: types::ToolCallGlobalConfig { workspace_dir },
            pending_tool_config: None,
            pending_tool_calls: HashMap::new(),
            in_flight_tool_calls: HashMap::new(),
            next_tool_queue_order: 0,
            last_model: None,
            last_provider: None,
            active_turn_profile: None,
            active_turn_auto_approve: false,
            active_turn_tool_state_snapshot: None,
        }
    }

    fn effective_workspace_dir(&self) -> PathBuf {
        self.pending_tool_config
            .as_ref()
            .unwrap_or(&self.active_tool_config)
            .workspace_dir
            .clone()
    }

    fn promote_pending_tool_config(&mut self) {
        if let Some(config) = self.pending_tool_config.take() {
            self.active_tool_config = config;
        }
    }
}

#[derive(Clone, Debug)]
struct StreamingMessageState {
    session_id: String,
    previous_tip: Option<MessageId>,
    message: Message,
}

pub struct AgentManager {
    providers: ProviderManager,
    tools: ToolManager,
    default_workspace_dir: PathBuf,
    message_store: Arc<dyn MessageStore>,
    session_store: Arc<dyn SessionStore>,
    session_states: HashMap<String, SessionRuntimeState>,
    last_used_profile_id: Option<String>,
    /// Messages currently being streamed (not yet persisted).
    streaming_messages: RwLock<HashMap<MessageId, StreamingMessageState>>,
}

#[derive(Clone, Debug)]
pub struct DetectedToolCall {
    pub call_id: CallId,
    pub tool_id: String,
    pub source_message_id: MessageId,
    pub description: String,
    pub assessment: ToolCallAssessment,
    pub requires_confirmation: bool,
    pub queue_order: u64,
}

impl AgentManager {
    pub fn new(
        providers: ProviderManager,
        tools: ToolManager,
        default_workspace_dir: PathBuf,
        message_store: Arc<dyn MessageStore>,
        session_store: Arc<dyn SessionStore>,
    ) -> Self {
        Self {
            providers,
            tools,
            default_workspace_dir,
            message_store,
            session_store,
            session_states: HashMap::new(),
            last_used_profile_id: None,
            streaming_messages: RwLock::new(HashMap::new()),
        }
    }

    pub async fn create_session(&mut self) -> Result<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_secs();

        let session_id = Ulid::new().to_string();
        let session = SessionMeta {
            id: session_id.clone(),
            tip_id: None,
            workspace_dir: self.default_workspace_dir.clone(),
            created_at: now,
            updated_at: now,
            title: None,
            selected_profile_id: Some(
                self.last_used_profile_id
                    .clone()
                    .unwrap_or_else(|| DEFAULT_AGENT_PROFILE_ID.to_string()),
            ),
        };

        self.session_store.save(&session).await?;
        self.ensure_runtime_state(&session_id, &session.workspace_dir);
        Ok(session_id)
    }

    pub async fn prepare_session(&mut self, session_id: &str) -> Result<bool> {
        match self.session_store.get(session_id).await? {
            Some(session) => {
                self.ensure_runtime_state(session_id, &session.workspace_dir);
                self.cleanup_hot_cache_for_session(&session).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Clean up hot cache, keeping only messages from the given session's tree.
    async fn cleanup_hot_cache_for_session(&self, session: &SessionMeta) -> Result<()> {
        let mut keep_ids = HashSet::new();

        if let Some(tip_id) = &session.tip_id {
            let mut current = Some(tip_id.clone());
            while let Some(id) = current {
                keep_ids.insert(id.clone());
                if let Some(msg) = self.message_store.get(&id).await? {
                    current = msg.parent_id;
                } else {
                    break;
                }
            }
        }

        let hot_ids = self.message_store.list_hot().await?;
        for id in hot_ids.difference(&keep_ids) {
            self.message_store.unload(id).await;
        }

        Ok(())
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionMeta>> {
        self.session_store.list().await
    }

    pub async fn delete_session(&mut self, session_id: &str) -> Result<()> {
        self.abort_streaming_messages_for_session(session_id)
            .await?;
        self.session_states.remove(session_id);
        self.session_store.delete(session_id).await
    }

    pub async fn set_workspace_dir(
        &mut self,
        session_id: &str,
        workspace_dir: PathBuf,
    ) -> Result<()> {
        let mut session = self.require_session(session_id).await?;
        session.workspace_dir = workspace_dir.clone();
        session.updated_at = current_unix_timestamp();
        self.session_store.save(&session).await?;

        self.ensure_runtime_state(session_id, &session.workspace_dir)
            .pending_tool_config = Some(types::ToolCallGlobalConfig { workspace_dir });
        Ok(())
    }

    pub async fn list_agent_profiles(&mut self, session_id: &str) -> Result<AgentProfilesState> {
        let session = self.require_session(session_id).await?;
        let resolved = self.resolve_profiles_for_workspace(&session.workspace_dir);
        let profile_locked = self.is_profile_locked(session_id);
        Ok(AgentProfilesState {
            profiles: resolved
                .profiles
                .iter()
                .map(AgentProfile::summary)
                .collect(),
            warnings: resolved.warnings,
            selected_profile_id: session.selected_profile_id,
            profile_locked,
        })
    }

    pub async fn set_session_profile(
        &mut self,
        session_id: &str,
        profile_id: String,
    ) -> Result<()> {
        if self.is_profile_locked(session_id) {
            return Err(eyre!(
                "Cannot change profile while the current turn is active"
            ));
        }

        let mut session = self.require_session(session_id).await?;
        let resolved = self.resolve_profiles_for_workspace(&session.workspace_dir);
        let exists = resolved
            .profiles
            .iter()
            .any(|profile| profile.id == profile_id);
        if !exists {
            return Err(eyre!("Unknown profile: {profile_id}"));
        }

        session.selected_profile_id = Some(profile_id);
        session.updated_at = current_unix_timestamp();
        self.session_store.save(&session).await?;
        Ok(())
    }

    pub async fn get_workspace_dir_state(
        &mut self,
        session_id: &str,
    ) -> Result<Option<(PathBuf, bool)>> {
        let Some(session) = self.session_store.get(session_id).await? else {
            return Ok(None);
        };

        let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
        Ok(Some((
            state.effective_workspace_dir(),
            state.pending_tool_config.is_some(),
        )))
    }

    pub fn is_profile_locked(&self, session_id: &str) -> bool {
        self.session_states
            .get(session_id)
            .is_some_and(|state| state.active_turn_profile.is_some())
    }

    pub async fn set_providers(
        &mut self,
        config: ProviderManagerConfig,
        registry: ProviderRegistry,
    ) -> Result<()> {
        self.providers.load_config(config, registry).await
    }

    pub async fn list_models(&self) -> HashMap<ProviderId, Vec<Model>> {
        self.providers.list_all_models().await
    }

    pub async fn get_tip(&self, session_id: &str) -> Result<Option<MessageId>> {
        Ok(self
            .session_store
            .get(session_id)
            .await?
            .and_then(|session| session.tip_id))
    }

    async fn set_tip(&self, session_id: &str, new_tip: Option<MessageId>) -> Result<()> {
        if let Some(mut session) = self.session_store.get(session_id).await? {
            session.tip_id = new_tip;
            session.updated_at = current_unix_timestamp();
            self.session_store.save(&session).await?;
        }

        Ok(())
    }

    async fn maybe_set_title_from_first_user_message(
        &self,
        session_id: &str,
        title: Option<String>,
    ) -> Result<()> {
        let Some(mut session) = self.session_store.get(session_id).await? else {
            return Ok(());
        };

        if session.title.is_some() {
            return Ok(());
        }

        let Some(title) = title else {
            return Ok(());
        };

        session.title = Some(title);
        session.updated_at = current_unix_timestamp();
        self.session_store.save(&session).await
    }

    async fn persist_tool_state_snapshot(
        &self,
        message_id: &MessageId,
        snapshot: ToolStateSnapshot,
    ) -> Result<()> {
        if let Some(mut message) = self.message_store.get(message_id).await? {
            message.tool_state_snapshot = Some(snapshot);
            self.message_store.save(&message).await?;
        }

        Ok(())
    }

    pub async fn prepare_start_stream(
        &mut self,
        session_id: &str,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> Result<PendingStreamRequest> {
        self.prepare_start_stream_with_options(session_id, message, model_id, provider_id, false)
            .await
    }

    pub async fn prepare_start_stream_with_options(
        &mut self,
        session_id: &str,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
        auto_approve: bool,
    ) -> Result<PendingStreamRequest> {
        let session = self.require_session(session_id).await?;
        let profile = self.resolve_selected_profile(&session)?;
        {
            let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
            if state.active_turn_profile.is_some() {
                return Err(eyre!(
                    "Cannot send a new message while the current turn is active"
                ));
            }
            state.promote_pending_tool_config();
            state.last_model = Some(model_id.clone());
            state.last_provider = Some(provider_id.clone());
            state.active_turn_profile = Some(profile.clone());
            state.active_turn_auto_approve = auto_approve;
        }
        self.last_used_profile_id = Some(profile.id.clone());

        let user_msg_id = match self
            .add_message(
                session_id,
                ChatRole::User,
                message,
                Some(profile.id.clone()),
            )
            .await
        {
            Ok(message_id) => message_id,
            Err(error) => {
                self.clear_active_turn(session_id);
                return Err(error);
            }
        };
        let context = match self.get_history_context(&user_msg_id).await {
            Ok(context) => context,
            Err(error) => {
                self.clear_active_turn(session_id);
                return Err(error);
            }
        };
        let mut tool_state_snapshot = resolve_snapshot_from_history(&context);
        let system_prompt = self.build_turn_system_prompt(
            session_id,
            &profile,
            &session.workspace_dir,
            &mut tool_state_snapshot,
        )?;
        if let Some(state) = self.session_states.get_mut(session_id) {
            state.active_turn_tool_state_snapshot = Some(tool_state_snapshot.clone());
        }
        if let Err(error) = self
            .persist_tool_state_snapshot(&user_msg_id, tool_state_snapshot.clone())
            .await
        {
            self.clear_active_turn(session_id);
            return Err(error);
        }

        let mut provider_messages: Vec<ChatMessage> = context
            .into_iter()
            .map(|m| ChatMessage {
                role: m.role,
                content: m.content,
            })
            .collect();

        if !system_prompt.is_empty() {
            provider_messages.push(ChatMessage {
                role: ChatRole::System,
                content: system_prompt,
            });
        }

        let call_id = CallId::new(Ulid::new());
        let assistant_msg_id = match self
            .start_streaming_message(session_id, ChatRole::Assistant, call_id, Some(profile.id))
            .await
        {
            Ok(message_id) => message_id,
            Err(error) => {
                self.clear_active_turn(session_id);
                return Err(error);
            }
        };

        Ok(PendingStreamRequest {
            message_id: assistant_msg_id,
            provider_id,
            model_id,
            provider_messages,
        })
    }

    pub async fn prepare_continuation_stream(
        &mut self,
        session_id: &str,
    ) -> Result<Option<PendingStreamRequest>> {
        let session = self.require_session(session_id).await?;
        let (model_id, provider_id, profile) = {
            let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
            let Some(model_id) = &state.last_model else {
                return Ok(None);
            };
            let Some(provider_id) = &state.last_provider else {
                return Ok(None);
            };
            let Some(profile) = &state.active_turn_profile else {
                return Ok(None);
            };
            (model_id.clone(), provider_id.clone(), profile.clone())
        };

        let tip_id = match self.get_tip(session_id).await? {
            Some(id) => id,
            None => return Ok(None),
        };

        let context = self.get_history_context(&tip_id).await?;
        let mut tool_state_snapshot = resolve_snapshot_from_history(&context);
        let system_prompt = self.build_turn_system_prompt(
            session_id,
            &profile,
            &session.workspace_dir,
            &mut tool_state_snapshot,
        )?;
        if let Some(state) = self.session_states.get_mut(session_id) {
            state.active_turn_tool_state_snapshot = Some(tool_state_snapshot.clone());
        }
        self.persist_tool_state_snapshot(&tip_id, tool_state_snapshot.clone())
            .await?;

        let mut provider_messages: Vec<ChatMessage> = context
            .into_iter()
            .map(|m| ChatMessage {
                role: m.role,
                content: m.content,
            })
            .collect();

        if !system_prompt.is_empty() {
            provider_messages.push(ChatMessage {
                role: ChatRole::System,
                content: system_prompt,
            });
        }

        let call_id = CallId::new(Ulid::new());
        let assistant_msg_id = self
            .start_streaming_message(session_id, ChatRole::Assistant, call_id, Some(profile.id))
            .await?;

        Ok(Some(PendingStreamRequest {
            message_id: assistant_msg_id,
            provider_id,
            model_id,
            provider_messages,
        }))
    }

    fn ensure_runtime_state(
        &mut self,
        session_id: &str,
        workspace_dir: &Path,
    ) -> &mut SessionRuntimeState {
        self.session_states
            .entry(session_id.to_string())
            .or_insert_with(|| SessionRuntimeState::new(workspace_dir.to_path_buf()))
    }

    fn resolve_profiles_for_workspace(&self, workspace_dir: &Path) -> ResolvedProfiles {
        let available_tools = self
            .tools
            .list_tools()
            .into_iter()
            .map(|tool_id| tool_id.to_string())
            .collect::<HashSet<_>>();
        resolve_profiles(workspace_dir, &available_tools)
    }

    fn resolve_selected_profile(&self, session: &SessionMeta) -> Result<AgentProfile> {
        let Some(profile_id) = session.selected_profile_id.as_ref() else {
            return Err(eyre!("No profile selected for this session"));
        };

        self.resolve_profiles_for_workspace(&session.workspace_dir)
            .profiles
            .into_iter()
            .find(|profile| &profile.id == profile_id)
            .ok_or_else(|| eyre!("Selected profile is unavailable: {profile_id}"))
    }

    fn build_system_prompt(&self, profile: &AgentProfile) -> Result<String> {
        let tool_prompt = self
            .tools
            .generate_system_prompt_for_tools(&profile.tools)
            .map_err(|error| eyre!(error.to_string()))?;
        if profile.system_prompt.is_empty() {
            Ok(tool_prompt)
        } else if tool_prompt.is_empty() {
            Ok(profile.system_prompt.clone())
        } else {
            Ok(format!("{}\n\n{}", profile.system_prompt, tool_prompt))
        }
    }

    fn build_turn_system_prompt(
        &self,
        session_id: &str,
        profile: &AgentProfile,
        workspace_dir: &Path,
        tool_state_snapshot: &mut ToolStateSnapshot,
    ) -> Result<String> {
        let mut sections = Vec::new();

        let base_system_prompt = self.build_system_prompt(profile)?;
        if !base_system_prompt.is_empty() {
            sections.push(base_system_prompt);
        }

        let tool_state_prompt = render_tool_state_prompt(tool_state_snapshot, workspace_dir);
        if !tool_state_prompt.is_empty() {
            sections.push(tool_state_prompt);
        }

        let system_prompt = sections.join("\n\n");
        #[cfg(debug_assertions)]
        {
            if system_prompt.is_empty() {
                tracing::info!(
                    session_id = session_id,
                    profile_id = %profile.id,
                    "Compiled turn system prompt is empty"
                );
            } else {
                tracing::info!(
                    session_id = session_id,
                    profile_id = %profile.id,
                    "Compiled turn system prompt:\n{}",
                    system_prompt
                );
            }
        }

        #[cfg(not(debug_assertions))]
        let _ = (session_id, profile, &system_prompt);

        Ok(system_prompt)
    }

    fn current_turn_profile_id(&self, session_id: &str) -> Option<String> {
        self.session_states
            .get(session_id)
            .and_then(|state| state.active_turn_profile.as_ref())
            .map(|profile| profile.id.clone())
    }

    async fn require_session(&self, session_id: &str) -> Result<SessionMeta> {
        self.session_store
            .get(session_id)
            .await?
            .ok_or_else(|| eyre!("Session not found: {session_id}"))
    }

    async fn add_message(
        &mut self,
        session_id: &str,
        role: ChatRole,
        content: String,
        agent_profile_id: Option<String>,
    ) -> Result<MessageId> {
        self.add_message_with_tool_state(
            session_id,
            role,
            content,
            agent_profile_id,
            None,
            Vec::new(),
        )
        .await
    }

    async fn add_message_with_tool_state(
        &mut self,
        session_id: &str,
        role: ChatRole,
        content: String,
        agent_profile_id: Option<String>,
        tool_state_snapshot: Option<ToolStateSnapshot>,
        tool_state_deltas: Vec<types::ToolStateDelta>,
    ) -> Result<MessageId> {
        self.require_session(session_id).await?;

        let message_id = MessageId::new(Ulid::new());
        let parent_id = self.get_tip(session_id).await?;
        let session_title = if role == ChatRole::User && parent_id.is_none() {
            title_from_user_prompt(&content)
        } else {
            None
        };

        tracing::debug!(
            "Adding message: session={}, id={}, role={:?}, parent={:?}",
            session_id,
            message_id,
            role,
            parent_id
        );

        let message = Message {
            id: message_id.clone(),
            parent_id,
            role,
            content,
            status: MessageStatus::Complete,
            agent_profile_id,
            tool_state_snapshot,
            tool_state_deltas,
        };

        self.message_store.save(&message).await?;
        self.set_tip(session_id, Some(message_id.clone())).await?;
        self.maybe_set_title_from_first_user_message(session_id, session_title)
        .await?;

        Ok(message_id)
    }

    async fn start_streaming_message(
        &mut self,
        session_id: &str,
        role: ChatRole,
        call_id: CallId,
        agent_profile_id: Option<String>,
    ) -> Result<MessageId> {
        self.require_session(session_id).await?;

        let session_has_stream = {
            let streaming = self.streaming_messages.read().await;
            streaming
                .values()
                .any(|state| state.session_id == session_id)
        };
        if session_has_stream {
            return Err(eyre!("Session already has an active stream: {session_id}"));
        }

        let message_id = MessageId::new(Ulid::new());
        let previous_tip = self.get_tip(session_id).await?;

        let message = Message {
            id: message_id.clone(),
            parent_id: previous_tip.clone(),
            role,
            content: String::new(),
            status: MessageStatus::Streaming { call_id },
            agent_profile_id,
            tool_state_snapshot: None,
            tool_state_deltas: Vec::new(),
        };

        let mut streaming = self.streaming_messages.write().await;
        streaming.insert(
            message_id.clone(),
            StreamingMessageState {
                session_id: session_id.to_string(),
                previous_tip,
                message,
            },
        );
        drop(streaming);

        self.set_tip(session_id, Some(message_id.clone())).await?;
        Ok(message_id)
    }

    pub async fn append_chunk(&self, message_id: &MessageId, chunk: &str) -> bool {
        let mut streaming = self.streaming_messages.write().await;
        if let Some(state) = streaming.get_mut(message_id) {
            state.message.content.push_str(chunk);
            true
        } else {
            false
        }
    }

    pub async fn complete_message(&self, message_id: &MessageId) -> Result<Option<String>> {
        let state = self.streaming_messages.write().await.remove(message_id);
        let Some(mut state) = state else {
            return Ok(None);
        };
        let original_state = state.clone();

        state.message.status = MessageStatus::Complete;
        if let Err(error) = self.message_store.save(&state.message).await {
            self.streaming_messages
                .write()
                .await
                .insert(message_id.clone(), original_state);
            return Err(error);
        }
        Ok(Some(state.session_id))
    }

    pub async fn abort_streaming_message(&self, message_id: &MessageId) -> Result<Option<String>> {
        let state = self.streaming_messages.write().await.remove(message_id);
        let Some(state) = state else {
            return Ok(None);
        };
        let original_state = state.clone();

        if let Err(error) = self
            .set_tip(&state.session_id, state.previous_tip.clone())
            .await
        {
            self.streaming_messages
                .write()
                .await
                .insert(message_id.clone(), original_state);
            return Err(error);
        }
        Ok(Some(state.session_id))
    }

    pub async fn cancel_streaming_message(
        &self,
        message_id: &MessageId,
    ) -> Result<Option<CancelledStreamResult>> {
        let state = self.streaming_messages.write().await.remove(message_id);
        let Some(mut state) = state else {
            return Ok(None);
        };

        let persisted = !state.message.content.is_empty();
        if persisted {
            state.message.status = MessageStatus::Complete;
            self.message_store.save(&state.message).await?;
        } else {
            self.set_tip(&state.session_id, state.previous_tip).await?;
        }

        Ok(Some(CancelledStreamResult {
            session_id: state.session_id,
            message_id: message_id.clone(),
            persisted,
        }))
    }

    async fn abort_streaming_messages_for_session(&self, session_id: &str) -> Result<()> {
        let to_abort: Vec<MessageId> = {
            let streaming = self.streaming_messages.read().await;
            streaming
                .iter()
                .filter_map(|(message_id, state)| {
                    (state.session_id == session_id).then_some(message_id.clone())
                })
                .collect()
        };

        for message_id in to_abort {
            self.abort_streaming_message(&message_id).await?;
        }

        Ok(())
    }

    async fn get_history_context(&self, from: &MessageId) -> Result<Vec<Message>> {
        let mut context = Vec::new();
        let mut current = Some(from.clone());

        while let Some(id) = current {
            {
                let streaming = self.streaming_messages.read().await;
                if let Some(state) = streaming.get(&id) {
                    context.push(state.message.clone());
                    current = state.message.parent_id.clone();
                    continue;
                }
            }

            if let Some(msg) = self.message_store.get(&id).await? {
                context.push(msg.clone());
                current = msg.parent_id.clone();
            } else {
                break;
            }
        }

        context.reverse();
        Ok(context)
    }

    pub async fn get_chat_history(&self, session_id: &str) -> Result<BTreeMap<MessageId, Message>> {
        let mut result = BTreeMap::new();

        let tip_id = match self.get_tip(session_id).await? {
            Some(id) => id,
            None => return Ok(result),
        };

        let context = self.get_history_context(&tip_id).await?;
        for msg in context {
            result.insert(msg.id.clone(), msg);
        }

        let streaming = self.streaming_messages.read().await;
        for (message_id, state) in streaming.iter() {
            if state.session_id == session_id {
                result.insert(message_id.clone(), state.message.clone());
            }
        }

        Ok(result)
    }

    pub async fn parse_tool_calls_from_content(
        &mut self,
        session_id: &str,
        source_message_id: &MessageId,
        content: &str,
    ) -> Result<(Vec<DetectedToolCall>, Vec<ParseFailure>)> {
        let session = self.require_session(session_id).await?;
        let (
            active_tool_config,
            active_turn_profile,
            active_turn_auto_approve,
            active_turn_tool_state_snapshot,
            mut next_queue_order,
        ) = {
            let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
            let Some(active_turn_profile) = state.active_turn_profile.clone() else {
                return Ok((Vec::new(), Vec::new()));
            };
            let active_turn_tool_state_snapshot = state
                .active_turn_tool_state_snapshot
                .clone()
                .unwrap_or_default();
            (
                state.active_tool_config.clone(),
                active_turn_profile,
                state.active_turn_auto_approve,
                active_turn_tool_state_snapshot,
                state.next_tool_queue_order,
            )
        };

        let result = toon_parser::parse_tool_calls(content);

        let mut detected_calls = Vec::new();
        let mut pending_tool_calls = Vec::new();
        let mut failed_calls = result.failed;
        let allowed_tools = active_turn_profile
            .tools
            .iter()
            .map(|tool_id| tool_id.as_str())
            .collect::<HashSet<_>>();

        for parsed in result.successful {
            let call_id = CallId::new(Ulid::new());
            let tool_id = ToolId::new(&parsed.tool_id);
            if !allowed_tools.contains(parsed.tool_id.as_str()) {
                failed_calls.push(ParseFailure {
                    kind: ParseFailureKind::ToolCall,
                    raw_content: parsed.raw_content,
                    error: format!(
                        "Tool '{}' is not allowed by the active profile '{}'",
                        parsed.tool_id, active_turn_profile.id
                    ),
                });
                continue;
            }
            let prepared = match self.tools.prepare_tool(&tool_id, parsed.args.clone()) {
                Ok(prepared) => prepared,
                Err(error) => {
                    failed_calls.push(ParseFailure {
                        kind: ParseFailureKind::ToolCall,
                        raw_content: parsed.raw_content,
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            let assessment = prepared.assess(&tool_core::ToolContext {
                global_config: &active_tool_config,
                tool_state_snapshot: &active_turn_tool_state_snapshot,
            });
            let description = prepared.describe();
            let requires_confirmation = !active_turn_auto_approve
                && !assessment.is_auto_approved(active_turn_profile.default_risk_level);

            let call = ToolCall {
                call_id: call_id.clone(),
                tool_id,
                args: parsed.args,
            };

            pending_tool_calls.push((
                call_id.clone(),
                PendingToolCall {
                    call,
                    source_message_id: source_message_id.clone(),
                    prepared,
                    description: description.clone(),
                    assessment: assessment.clone(),
                    config: active_tool_config.clone(),
                    tool_state_snapshot: active_turn_tool_state_snapshot.clone(),
                    status: if requires_confirmation {
                        PermissionStatus::Pending
                    } else {
                        PermissionStatus::Approved
                    },
                    queue_order: next_queue_order,
                },
            ));

            detected_calls.push(DetectedToolCall {
                call_id,
                tool_id: parsed.tool_id,
                source_message_id: source_message_id.clone(),
                description,
                assessment,
                requires_confirmation,
                queue_order: next_queue_order,
            });
            next_queue_order = next_queue_order.saturating_add(1);
        }

        let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
        state.pending_tool_calls.extend(pending_tool_calls);
        state.next_tool_queue_order = next_queue_order;

        Ok((detected_calls, failed_calls))
    }

    pub async fn add_parse_failures_to_history(
        &mut self,
        session_id: &str,
        failed: Vec<ParseFailure>,
    ) -> Result<()> {
        let agent_profile_id = self.current_turn_profile_id(session_id);
        for failure in failed {
            let content = match failure.kind {
                ParseFailureKind::ToolCall => format!(
                    "Failed to parse tool call:\n```\n{}\n```\nError: {}",
                    failure.raw_content, failure.error
                ),
                ParseFailureKind::ThinkingBlock => format!(
                    "Failed to parse thinking block:\n```\n{}\n```\nError: {}",
                    failure.raw_content, failure.error
                ),
            };
            self.add_message_with_tool_state(
                session_id,
                ChatRole::Tool,
                content,
                agent_profile_id.clone(),
                None,
                Vec::new(),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn process_message_for_tools(
        &mut self,
        session_id: &str,
        message_id: &MessageId,
    ) -> Result<Vec<DetectedToolCall>> {
        let history = self.get_chat_history(session_id).await?;
        let message = match history.get(message_id) {
            Some(message) => message,
            None => return Ok(vec![]),
        };

        let (detected, failed) = self
            .parse_tool_calls_from_content(session_id, message_id, &message.content)
            .await?;

        if !failed.is_empty() {
            self.add_parse_failures_to_history(session_id, failed)
                .await?;
        }

        Ok(detected)
    }

    pub fn approve_tool(&mut self, session_id: &str, call_id: CallId) -> bool {
        self.session_states
            .get_mut(session_id)
            .and_then(|state| state.pending_tool_calls.get_mut(&call_id))
            .map(|pending| {
                pending.status = PermissionStatus::Approved;
            })
            .is_some()
    }

    pub fn deny_tool(&mut self, session_id: &str, call_id: CallId) -> bool {
        self.session_states
            .get_mut(session_id)
            .and_then(|state| state.pending_tool_calls.get_mut(&call_id))
            .map(|pending| {
                pending.status = PermissionStatus::Denied;
            })
            .is_some()
    }

    pub fn list_pending_tools(&self, session_id: &str) -> Vec<PendingToolInfo> {
        let Some(state) = self.session_states.get(session_id) else {
            return Vec::new();
        };

        let mut tools: Vec<_> = state
            .pending_tool_calls
            .values()
            .map(|pending| PendingToolInfo {
                call_id: pending.call.call_id.clone(),
                tool_id: pending.call.tool_id.clone(),
                args: pending.call.args.clone(),
                description: pending.description.clone(),
                risk_level: pending.assessment.risk,
                reasons: pending.assessment.reasons.clone(),
                approved: match pending.status {
                    PermissionStatus::Pending => None,
                    PermissionStatus::Approved => Some(true),
                    PermissionStatus::Denied => Some(false),
                },
                queue_order: pending.queue_order,
            })
            .collect();
        tools.sort_by_key(|tool| tool.queue_order);
        tools
    }

    pub fn session_waiting_for_approval(&self, session_id: &str) -> bool {
        self.session_states.get(session_id).is_some_and(|state| {
            state
                .pending_tool_calls
                .values()
                .any(|pending| pending.status == PermissionStatus::Pending)
        })
    }

    pub fn clear_active_turn(&mut self, session_id: &str) {
        if let Some(state) = self.session_states.get_mut(session_id) {
            state.active_turn_profile = None;
            state.active_turn_auto_approve = false;
            state.active_turn_tool_state_snapshot = None;
        }
    }

    pub fn is_turn_active(&self, session_id: &str) -> bool {
        self.session_states
            .get(session_id)
            .is_some_and(|state| state.active_turn_profile.is_some())
    }

    pub async fn streaming_session_ids(&self) -> HashSet<String> {
        self.streaming_messages
            .read()
            .await
            .values()
            .map(|state| state.session_id.clone())
            .collect()
    }

    pub fn cloned_tool_manager(&self) -> ToolManager {
        self.tools.clone()
    }

    pub fn cloned_provider_manager(&self) -> ProviderManager {
        self.providers.clone()
    }

    pub fn take_ready_tool_executions(&mut self, session_id: &str) -> Vec<ToolExecutionRequest> {
        let Some(state) = self.session_states.get_mut(session_id) else {
            return Vec::new();
        };

        let ready_ids: Vec<_> = state
            .pending_tool_calls
            .iter()
            .filter(|(_, pending)| pending.status != PermissionStatus::Pending)
            .map(|(call_id, _)| call_id.clone())
            .collect();

        let mut executions = Vec::new();
        for call_id in ready_ids {
            let Some(pending) = state.pending_tool_calls.remove(&call_id) else {
                continue;
            };
            *state
                .in_flight_tool_calls
                .entry(pending.source_message_id.clone())
                .or_default() += 1;

            match pending.status {
                PermissionStatus::Pending => {}
                PermissionStatus::Approved => executions.push(ToolExecutionRequest {
                    call_id,
                    tool_id: pending.call.tool_id,
                    source_message_id: pending.source_message_id,
                    payload: ToolExecutionPayload::Approved {
                        prepared: pending.prepared,
                        config: pending.config,
                        tool_state_snapshot: pending.tool_state_snapshot,
                    },
                }),
                PermissionStatus::Denied => executions.push(ToolExecutionRequest {
                    call_id,
                    tool_id: pending.call.tool_id,
                    source_message_id: pending.source_message_id,
                    payload: ToolExecutionPayload::Denied,
                }),
            }
        }

        executions
    }

    pub fn finish_tool_executions(&mut self, session_id: &str, source_message_ids: &[MessageId]) {
        let Some(state) = self.session_states.get_mut(session_id) else {
            return;
        };

        for source_message_id in source_message_ids {
            let mut should_remove = false;
            if let Some(in_flight) = state.in_flight_tool_calls.get_mut(source_message_id) {
                if *in_flight > 0 {
                    *in_flight -= 1;
                }
                should_remove = *in_flight == 0;
            }
            if should_remove {
                state.in_flight_tool_calls.remove(source_message_id);
            }
        }
    }

    pub async fn add_tool_results_to_history(
        &mut self,
        session_id: &str,
        results: Vec<ToolResult>,
    ) -> Result<()> {
        let agent_profile_id = self.current_turn_profile_id(session_id);
        for result in results {
            let content = types::format_tool_result_message(
                &result.tool_id,
                &result.output,
                result.permission_denied,
            );

            tracing::debug!(
                "Adding tool result to history: session={}, tool_id={}, denied={}",
                session_id,
                result.tool_id,
                result.permission_denied
            );

            self.add_message_with_tool_state(
                session_id,
                ChatRole::Tool,
                content,
                agent_profile_id.clone(),
                None,
                result.tool_state_deltas,
            )
            .await?;
        }
        Ok(())
    }

    pub fn has_pending_tools(&self, session_id: &str) -> bool {
        self.session_states
            .get(session_id)
            .is_some_and(|state| !state.pending_tool_calls.is_empty())
    }

    pub fn has_unfinished_tools_for_message(
        &self,
        session_id: &str,
        source_message_id: &MessageId,
    ) -> bool {
        self.session_states.get(session_id).is_some_and(|state| {
            state
                .pending_tool_calls
                .values()
                .any(|pending| &pending.source_message_id == source_message_id)
                || state
                    .in_flight_tool_calls
                    .get(source_message_id)
                    .is_some_and(|in_flight| *in_flight > 0)
        })
    }

    pub fn get_pending_tool_args(
        &self,
        session_id: &str,
        call_id: &CallId,
    ) -> Option<serde_json::Value> {
        self.session_states
            .get(session_id)
            .and_then(|state| state.pending_tool_calls.get(call_id))
            .map(|pending| pending.call.args.clone())
    }

    pub fn get_pending_tool_assessment(
        &self,
        session_id: &str,
        call_id: &CallId,
    ) -> Option<ToolCallAssessment> {
        self.session_states
            .get(session_id)
            .and_then(|state| state.pending_tool_calls.get(call_id))
            .map(|pending| pending.assessment.clone())
    }
}

fn current_unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use color_eyre::eyre::Result;
    use futures::stream::BoxStream;
    use provider_core::Provider;
    use serde::Deserialize;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tool_core::{ToolCallResult, ToolContext, TypedTool};
    use types::{
        ChatMessage as ProviderChatMessage, ExecutionPolicy, MessageStatus, ToolResult,
        ToolStateDelta,
    };

    struct MockProvider {
        id: ProviderId,
    }

    #[derive(Clone, Deserialize)]
    struct MockToolArgs {}

    #[derive(Clone)]
    struct MockTool {
        name: &'static str,
    }

    #[async_trait]
    impl TypedTool for MockTool {
        type Args = MockToolArgs;

        fn name(&self) -> &'static str {
            self.name
        }

        fn schema(&self) -> &'static str {
            "mock schema"
        }

        async fn call(&self, _args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
            ToolCallResult::success(serde_json::json!({ "ok": true }))
        }
    }

    #[async_trait::async_trait]
    impl Provider for MockProvider {
        fn get_provider_id(&self) -> ProviderId {
            self.id.clone()
        }

        async fn list_models(&self) -> Vec<Model> {
            vec![Model {
                id: ModelId::new("mock-model"),
                name: String::from("Mock Model"),
                max_context: None,
            }]
        }

        async fn cache_models(&self) -> Result<()> {
            Ok(())
        }

        async fn register_model(&mut self, _model: provider_core::ModelConfig) -> Result<()> {
            Ok(())
        }

        async fn generate_reply(
            &self,
            _model_id: &ModelId,
            _messages: Vec<ProviderChatMessage>,
            _request_context: &provider_core::ProviderRequestContext,
        ) -> Result<ProviderChatMessage> {
            Ok(ProviderChatMessage {
                role: ChatRole::Assistant,
                content: String::from("reply"),
            })
        }

        async fn generate_reply_stream(
            &self,
            _model_id: &ModelId,
            _messages: Vec<ProviderChatMessage>,
            _request_context: &provider_core::ProviderRequestContext,
        ) -> Result<BoxStream<'static, Result<String>>> {
            Ok(Box::pin(futures::stream::iter(vec![Ok(String::from(
                "reply",
            ))])))
        }
    }

    fn test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("agent-core-{name}-{nanos}-{}", Ulid::new()))
    }

    async fn test_manager() -> (AgentManager, PathBuf) {
        let data_dir = test_dir("manager");
        tokio::fs::create_dir_all(&data_dir).await.unwrap();

        let message_store = Arc::new(persistence::FileMessageStore::new(&data_dir));
        let session_store = Arc::new(persistence::FileSessionStore::new(
            &data_dir,
            message_store.clone(),
        ));

        let mut providers = ProviderManager::new();
        providers.register_provider(
            ProviderId::new("mock"),
            Box::new(MockProvider {
                id: ProviderId::new("mock"),
            }),
        );

        let mut tools = ToolManager::new();
        tools.register_tool(MockTool { name: "close_file" });
        tools.register_tool(MockTool { name: "list_files" });
        tools.register_tool(MockTool { name: "open_file" });
        tools.register_tool(MockTool {
            name: "search_files",
        });
        tools.register_tool(MockTool { name: "read_files" });
        tools.register_tool(MockTool { name: "edit_file" });

        let manager = AgentManager::new(
            providers,
            tools,
            PathBuf::from("/tmp/default-workspace"),
            message_store,
            session_store,
        );
        (manager, data_dir)
    }

    async fn cleanup_dir(data_dir: PathBuf) {
        let _ = tokio::fs::remove_dir_all(data_dir).await;
    }

    fn open_file_state_delta(path: &Path) -> ToolStateDelta {
        ToolStateDelta {
            namespace: String::from("opened_files"),
            operation: String::from("open"),
            payload: json!({ "path": path.display().to_string() }),
        }
    }

    #[tokio::test]
    async fn create_session_returns_usable_session_id() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let session_id = manager.create_session().await?;
        let sessions = manager.list_sessions().await?;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session_id);
        assert_eq!(
            sessions[0].selected_profile_id.as_deref(),
            Some("plan-code")
        );
        assert_eq!(manager.get_tip(&session_id).await?, None);

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[test]
    fn title_from_user_prompt_truncates_to_sixty_characters() {
        let prompt = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let title = title_from_user_prompt(prompt).expect("title should be present");

        assert_eq!(title, "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01234567");
        assert_eq!(title.chars().count(), 60);
    }

    #[test]
    fn title_from_user_prompt_flattens_newlines() {
        let title = title_from_user_prompt("first line\nsecond\r\nthird")
            .expect("title should be present");

        assert_eq!(title, "first line second third");
        assert!(!title.contains('\n'));
        assert!(!title.contains('\r'));
    }

    #[tokio::test]
    async fn last_used_profile_is_inherited_by_new_sessions() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let first_session = manager.create_session().await?;
        manager
            .set_session_profile(&first_session, String::from("plan-code"))
            .await?;
        let _request = manager
            .prepare_start_stream(
                &first_session,
                String::from("hello"),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await?;

        let second_session = manager.create_session().await?;
        let sessions = manager.list_sessions().await?;
        let inherited = sessions
            .into_iter()
            .find(|session| session.id == second_session)
            .unwrap();
        assert_eq!(inherited.selected_profile_id.as_deref(), Some("plan-code"));

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn profile_changes_are_rejected_while_turn_is_active() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let session_id = manager.create_session().await?;
        manager
            .set_session_profile(&session_id, String::from("plan-code"))
            .await?;
        let _request = manager
            .prepare_start_stream(
                &session_id,
                String::from("hello"),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await?;

        let locked = manager
            .set_session_profile(&session_id, String::from("build-code"))
            .await;
        assert!(locked.is_err());

        manager.clear_active_turn(&session_id);
        manager
            .set_session_profile(&session_id, String::from("build-code"))
            .await?;

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn sessions_keep_independent_tips_and_histories() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let session_a = manager.create_session().await?;
        let session_b = manager.create_session().await?;

        let a_message = manager
            .add_message(&session_a, ChatRole::User, String::from("hello a"), None)
            .await?;
        let b_message = manager
            .add_message(&session_b, ChatRole::User, String::from("hello b"), None)
            .await?;

        assert_eq!(manager.get_tip(&session_a).await?, Some(a_message.clone()));
        assert_eq!(manager.get_tip(&session_b).await?, Some(b_message.clone()));

        let history_a = manager.get_chat_history(&session_a).await?;
        let history_b = manager.get_chat_history(&session_b).await?;

        assert_eq!(history_a.len(), 1);
        assert_eq!(history_b.len(), 1);
        assert_eq!(history_a.get(&a_message).unwrap().content, "hello a");
        assert_eq!(history_b.get(&b_message).unwrap().content, "hello b");

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn first_user_message_sets_session_title() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let session_id = manager.create_session().await?;
        manager
            .add_message(
                &session_id,
                ChatRole::User,
                String::from(
                    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
                ),
                None,
            )
            .await?;

        let session = manager.require_session(&session_id).await?;
        assert_eq!(
            session.title.as_deref(),
            Some("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01234567")
        );

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn later_user_messages_do_not_overwrite_session_title() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let session_id = manager.create_session().await?;
        manager
            .add_message(
                &session_id,
                ChatRole::User,
                String::from("first prompt"),
                None,
            )
            .await?;
        manager
            .add_message(
                &session_id,
                ChatRole::Assistant,
                String::from("assistant response"),
                None,
            )
            .await?;
        manager
            .add_message(
                &session_id,
                ChatRole::User,
                String::from("second prompt should not replace the title"),
                None,
            )
            .await?;

        let session = manager.require_session(&session_id).await?;
        assert_eq!(session.title.as_deref(), Some("first prompt"));

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn start_stream_failure_rolls_tip_back_to_last_durable_message() -> Result<()> {
        let data_dir = test_dir("stream-failure");
        tokio::fs::create_dir_all(&data_dir).await.unwrap();

        let message_store = Arc::new(persistence::FileMessageStore::new(&data_dir));
        let session_store = Arc::new(persistence::FileSessionStore::new(
            &data_dir,
            message_store.clone(),
        ));
        let manager_providers = ProviderManager::new();
        let mut tools = ToolManager::new();
        tools.register_tool(MockTool { name: "close_file" });
        tools.register_tool(MockTool { name: "list_files" });
        tools.register_tool(MockTool { name: "open_file" });
        tools.register_tool(MockTool {
            name: "search_files",
        });
        tools.register_tool(MockTool { name: "read_files" });
        tools.register_tool(MockTool { name: "edit_file" });
        let mut manager = AgentManager::new(
            manager_providers,
            tools,
            PathBuf::from("/tmp/default-workspace"),
            message_store,
            session_store,
        );

        let session_id = manager.create_session().await?;
        manager
            .set_session_profile(&session_id, String::from("plan-code"))
            .await?;
        manager
            .add_message(&session_id, ChatRole::User, String::from("hello"), None)
            .await?;

        let request = manager
            .prepare_start_stream(
                &session_id,
                String::from("trigger failure"),
                ModelId::new("mock-model"),
                ProviderId::new("missing-provider"),
            )
            .await?;
        let result = manager
            .cloned_provider_manager()
            .generate_reply_stream(
                request.provider_id,
                &request.model_id,
                request.provider_messages,
                provider_core::ProviderRequestContext::default(),
            )
            .await;
        assert!(result.is_err());
        manager.abort_streaming_message(&request.message_id).await?;

        let tip = manager.get_tip(&session_id).await?;
        let history = manager.get_chat_history(&session_id).await?;
        let latest_user_message = history
            .values()
            .find(|message| message.role == ChatRole::User && message.content == "trigger failure")
            .unwrap();

        assert_eq!(tip, Some(latest_user_message.id.clone()));
        assert_eq!(history.len(), 2);
        assert!(
            history
                .values()
                .all(|message| message.status == MessageStatus::Complete)
        );

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn deleting_session_aborts_stream_and_removes_transient_state() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let session_id = manager.create_session().await?;
        let stable_tip = manager
            .add_message(
                &session_id,
                ChatRole::User,
                String::from("before stream"),
                None,
            )
            .await?;
        let streaming_id = manager
            .start_streaming_message(
                &session_id,
                ChatRole::Assistant,
                CallId::new("call-1"),
                None,
            )
            .await?;

        assert_eq!(
            manager.get_tip(&session_id).await?,
            Some(streaming_id.clone())
        );

        manager.delete_session(&session_id).await?;

        assert!(manager.get_tip(&session_id).await?.is_none());
        assert!(manager.get_chat_history(&session_id).await?.is_empty());
        assert!(
            manager
                .streaming_messages
                .read()
                .await
                .get(&streaming_id)
                .is_none()
        );
        assert!(!manager.message_store.exists(&stable_tip).await?);

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn workspace_and_pending_tools_are_isolated_per_session() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let session_a = manager.create_session().await?;
        let session_b = manager.create_session().await?;

        manager
            .set_workspace_dir(&session_a, PathBuf::from("/tmp/workspace-a"))
            .await?;

        let call_id = CallId::new("call-a");
        manager
            .session_states
            .get_mut(&session_a)
            .unwrap()
            .pending_tool_calls
            .insert(
                call_id.clone(),
                PendingToolCall {
                    call: ToolCall {
                        call_id: call_id.clone(),
                        tool_id: ToolId::new("list_files"),
                        args: serde_json::json!({ "path": "." }),
                    },
                    source_message_id: MessageId::new("msg-a"),
                    prepared: manager
                        .tools
                        .prepare_tool(
                            &ToolId::new("list_files"),
                            serde_json::json!({ "path": "." }),
                        )
                        .expect("prepare list_files tool"),
                    description: String::from("test"),
                    assessment: ToolCallAssessment {
                        risk: RiskLevel::ReadOnlyWorkspace,
                        policy: ExecutionPolicy::AlwaysAsk,
                        reasons: Vec::new(),
                    },
                    config: types::ToolCallGlobalConfig {
                        workspace_dir: PathBuf::from("/tmp/workspace-a"),
                    },
                    tool_state_snapshot: ToolStateSnapshot::default(),
                    status: PermissionStatus::Pending,
                    queue_order: 0,
                },
            );

        let workspace_a = manager.get_workspace_dir_state(&session_a).await?.unwrap();
        let workspace_b = manager.get_workspace_dir_state(&session_b).await?.unwrap();

        assert_eq!(workspace_a.0, PathBuf::from("/tmp/workspace-a"));
        assert!(workspace_a.1);
        assert_eq!(workspace_b.0, PathBuf::from("/tmp/default-workspace"));
        assert!(!workspace_b.1);
        assert!(manager.has_pending_tools(&session_a));
        assert!(!manager.has_pending_tools(&session_b));
        assert!(!manager.approve_tool(&session_b, call_id.clone()));
        assert!(manager.approve_tool(&session_a, call_id));

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn new_sessions_default_to_plan_code_on_fresh_manager() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let session_id = manager.create_session().await?;
        let session = manager
            .list_sessions()
            .await?
            .into_iter()
            .find(|session| session.id == session_id)
            .unwrap();

        assert_eq!(session.selected_profile_id.as_deref(), Some("plan-code"));

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn new_sessions_inherit_last_used_profile_after_turn_starts() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let first_session = manager.create_session().await?;
        manager
            .set_session_profile(&first_session, String::from("build-code"))
            .await?;
        let pending = manager
            .prepare_start_stream(
                &first_session,
                String::from("build something"),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await?;
        manager.abort_streaming_message(&pending.message_id).await?;
        manager.clear_active_turn(&first_session);

        let second_session = manager.create_session().await?;
        let inherited = manager
            .list_sessions()
            .await?
            .into_iter()
            .find(|session| session.id == second_session)
            .unwrap();

        assert_eq!(inherited.selected_profile_id.as_deref(), Some("build-code"));

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn prepare_start_stream_fails_when_no_profile_is_selected() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let session_id = manager.create_session().await?;
        let mut session = manager
            .session_store
            .get(&session_id)
            .await?
            .expect("session should exist");
        session.selected_profile_id = None;
        manager.session_store.save(&session).await?;
        let error = manager
            .prepare_start_stream(
                &session_id,
                String::from("hello"),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("No profile selected"));

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn prepare_start_stream_persists_snapshot_on_user_tip_and_injects_latest_open_file()
    -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;
        let workspace_dir = test_dir("open-file-start");
        tokio::fs::create_dir_all(&workspace_dir).await?;
        let file_path = workspace_dir.join("notes.txt");
        let file_path_str = file_path.display().to_string();
        tokio::fs::write(&file_path, "old contents\n").await?;

        let session_id = manager.create_session().await?;
        manager
            .set_workspace_dir(&session_id, workspace_dir.clone())
            .await?;
        manager
            .set_session_profile(&session_id, String::from("plan-code"))
            .await?;
        manager
            .add_message(&session_id, ChatRole::User, String::from("prior"), None)
            .await?;
        manager
            .add_tool_results_to_history(
                &session_id,
                vec![ToolResult {
                    call_id: CallId::new("open-call"),
                    tool_id: ToolId::new("open_file"),
                    output: json!({ "success": true, "path": file_path_str.clone() }),
                    permission_denied: false,
                    tool_state_deltas: vec![open_file_state_delta(&file_path)],
                }],
            )
            .await?;
        tokio::fs::write(&file_path, "new contents\nsecond line\n").await?;

        let request = manager
            .prepare_start_stream(
                &session_id,
                String::from("follow up"),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await?;

        let history = manager.get_chat_history(&session_id).await?;
        let user_message = history
            .values()
            .find(|message| message.role == ChatRole::User && message.content == "follow up")
            .expect("new user message should exist");
        let snapshot = user_message
            .tool_state_snapshot
            .as_ref()
            .expect("user message should store tool state snapshot");
        assert_eq!(
            snapshot.entries["opened_files"]["paths"][0].as_str(),
            Some(file_path_str.as_str())
        );
        assert!(
            snapshot.entries["file_reads"]["by_path"][file_path_str.as_str()]
                .as_str()
                .is_some()
        );

        let system_prompt = request
            .provider_messages
            .iter()
            .rev()
            .find(|message| message.role == ChatRole::System)
            .expect("system prompt should be present");
        assert!(system_prompt.content.contains("Opened Files"));
        assert!(system_prompt.content.contains(file_path_str.as_str()));
        assert!(system_prompt.content.contains("1|new contents"));
        assert!(system_prompt.content.contains("2|second line"));

        let _ = tokio::fs::remove_dir_all(&workspace_dir).await;
        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn prepare_continuation_persists_snapshot_on_tool_tip() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;
        let workspace_dir = test_dir("open-file-continuation");
        tokio::fs::create_dir_all(&workspace_dir).await?;
        let file_path = workspace_dir.join("notes.txt");
        let file_path_str = file_path.display().to_string();
        tokio::fs::write(&file_path, "current\n").await?;

        let session_id = manager.create_session().await?;
        manager
            .set_workspace_dir(&session_id, workspace_dir.clone())
            .await?;
        manager
            .set_session_profile(&session_id, String::from("plan-code"))
            .await?;
        manager
            .add_message(&session_id, ChatRole::User, String::from("prior"), None)
            .await?;
        manager
            .add_tool_results_to_history(
                &session_id,
                vec![ToolResult {
                    call_id: CallId::new("open-call"),
                    tool_id: ToolId::new("open_file"),
                    output: json!({ "success": true, "path": file_path_str.clone() }),
                    permission_denied: false,
                    tool_state_deltas: vec![open_file_state_delta(&file_path)],
                }],
            )
            .await?;

        let session = manager.require_session(&session_id).await?;
        let profile = manager.resolve_selected_profile(&session)?;
        let state = manager.ensure_runtime_state(&session_id, &session.workspace_dir);
        state.last_model = Some(ModelId::new("mock-model"));
        state.last_provider = Some(ProviderId::new("mock"));
        state.active_turn_profile = Some(profile);

        let request = manager
            .prepare_continuation_stream(&session_id)
            .await?
            .expect("continuation request should exist");

        let history = manager.get_chat_history(&session_id).await?;
        let tool_message = history
            .values()
            .find(|message| message.role == ChatRole::Tool && !message.tool_state_deltas.is_empty())
            .expect("tool result message should exist");
        let snapshot = tool_message
            .tool_state_snapshot
            .as_ref()
            .expect("tool result message should store tool state snapshot");
        assert_eq!(
            snapshot.entries["opened_files"]["paths"][0].as_str(),
            Some(file_path_str.as_str())
        );
        assert!(
            snapshot.entries["file_reads"]["by_path"][file_path_str.as_str()]
                .as_str()
                .is_some()
        );

        let system_prompt = request
            .provider_messages
            .iter()
            .rev()
            .find(|message| message.role == ChatRole::System)
            .expect("system prompt should be present");
        assert!(system_prompt.content.contains("1|current"));

        let _ = tokio::fs::remove_dir_all(&workspace_dir).await;
        cleanup_dir(data_dir).await;
        Ok(())
    }
}
