use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use persistence::{MessageStore, SessionMeta, SessionStore};
use provider_core::{Model, ProviderManager, ProviderManagerConfig, ProviderManagerHelper};
use tokio::sync::RwLock;
use tool_core::{ToolManager, toon_parser};
use types::{
    CallId, ChatMessage, ChatRole, Message, MessageId, MessageStatus, ModelId, ProviderId,
    RiskLevel, ToolCall, ToolCallAssessment, ToolId, ToolResult,
};
use ulid::Ulid;

#[derive(Clone, Debug)]
pub struct PendingToolCall {
    pub call: ToolCall,
    pub description: String,
    pub assessment: ToolCallAssessment,
    pub config: types::ToolCallGlobalConfig,
    pub status: PermissionStatus,
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
}

#[derive(Clone, Debug)]
pub struct ToolExecutionRequest {
    pub call_id: CallId,
    pub tool_id: ToolId,
    pub args: Option<serde_json::Value>,
    pub config: Option<types::ToolCallGlobalConfig>,
    pub permission_denied: bool,
}

#[derive(Clone, Debug)]
pub struct PendingStreamRequest {
    pub message_id: MessageId,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub provider_messages: Vec<ChatMessage>,
}

#[derive(Clone, Debug)]
struct SessionRuntimeState {
    active_tool_config: types::ToolCallGlobalConfig,
    pending_tool_config: Option<types::ToolCallGlobalConfig>,
    pending_tool_calls: HashMap<CallId, PendingToolCall>,
    last_model: Option<ModelId>,
    last_provider: Option<ProviderId>,
}

impl SessionRuntimeState {
    fn new(workspace_dir: PathBuf) -> Self {
        Self {
            active_tool_config: types::ToolCallGlobalConfig { workspace_dir },
            pending_tool_config: None,
            pending_tool_calls: HashMap::new(),
            last_model: None,
            last_provider: None,
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
    /// Messages currently being streamed (not yet persisted).
    streaming_messages: RwLock<HashMap<MessageId, StreamingMessageState>>,
}

#[derive(Clone, Debug)]
pub struct DetectedToolCall {
    pub call_id: CallId,
    pub tool_id: String,
    pub description: String,
    pub assessment: ToolCallAssessment,
    pub requires_confirmation: bool,
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

    pub async fn set_providers(
        &mut self,
        config: ProviderManagerConfig,
        helper: ProviderManagerHelper,
    ) -> Result<()> {
        self.providers.load_config(config, helper).await
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

    pub async fn prepare_start_stream(
        &mut self,
        session_id: &str,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> Result<PendingStreamRequest> {
        let session = self.require_session(session_id).await?;
        {
            let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
            state.promote_pending_tool_config();
            state.last_model = Some(model_id.clone());
            state.last_provider = Some(provider_id.clone());
        }

        let user_msg_id = self
            .add_message(session_id, ChatRole::User, message)
            .await?;
        let context = self.get_history_context(&user_msg_id).await?;

        let mut provider_messages: Vec<ChatMessage> = context
            .into_iter()
            .map(|m| ChatMessage {
                role: m.role,
                content: m.content,
            })
            .collect();

        let system_prompt = self.tools.generate_system_prompt();
        if !system_prompt.is_empty() {
            provider_messages.push(ChatMessage {
                role: ChatRole::System,
                content: system_prompt,
            });
        }

        let call_id = CallId::new(Ulid::new());
        let assistant_msg_id = self
            .start_streaming_message(session_id, ChatRole::Assistant, call_id)
            .await?;

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
        let (model_id, provider_id) = {
            let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
            let Some(model_id) = &state.last_model else {
                return Ok(None);
            };
            let Some(provider_id) = &state.last_provider else {
                return Ok(None);
            };
            (model_id.clone(), provider_id.clone())
        };

        let tip_id = match self.get_tip(session_id).await? {
            Some(id) => id,
            None => return Ok(None),
        };

        let context = self.get_history_context(&tip_id).await?;

        let mut provider_messages: Vec<ChatMessage> = context
            .into_iter()
            .map(|m| ChatMessage {
                role: m.role,
                content: m.content,
            })
            .collect();

        let system_prompt = self.tools.generate_system_prompt();
        if !system_prompt.is_empty() {
            provider_messages.push(ChatMessage {
                role: ChatRole::System,
                content: system_prompt,
            });
        }

        let call_id = CallId::new(Ulid::new());
        let assistant_msg_id = self
            .start_streaming_message(session_id, ChatRole::Assistant, call_id)
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
    ) -> Result<MessageId> {
        self.require_session(session_id).await?;

        let message_id = MessageId::new(Ulid::new());
        let parent_id = self.get_tip(session_id).await?;

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
        };

        self.message_store.save(&message).await?;
        self.set_tip(session_id, Some(message_id.clone())).await?;

        Ok(message_id)
    }

    async fn start_streaming_message(
        &mut self,
        session_id: &str,
        role: ChatRole,
        call_id: CallId,
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

        state.message.status = MessageStatus::Complete;
        self.message_store.save(&state.message).await?;
        Ok(Some(state.session_id))
    }

    pub async fn abort_streaming_message(&self, message_id: &MessageId) -> Result<Option<String>> {
        let state = self.streaming_messages.write().await.remove(message_id);
        let Some(state) = state else {
            return Ok(None);
        };

        self.set_tip(&state.session_id, state.previous_tip).await?;
        Ok(Some(state.session_id))
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
        content: &str,
    ) -> Result<(Vec<DetectedToolCall>, Vec<(String, String)>)> {
        let session = self.require_session(session_id).await?;
        let active_tool_config = self
            .ensure_runtime_state(session_id, &session.workspace_dir)
            .active_tool_config
            .clone();

        let result = toon_parser::parse_tool_calls(content);

        let mut detected_calls = Vec::new();
        let mut pending_tool_calls = Vec::new();

        for parsed in result.successful {
            let call_id = CallId::new(Ulid::new());
            let tool_id = ToolId::new(&parsed.tool_id);
            let assessment = self
                .tools
                .assess_tool(
                    &tool_id,
                    &parsed.args,
                    &tool_core::ToolContext {
                        global_config: &active_tool_config,
                    },
                )
                .unwrap_or_else(|_| ToolCallAssessment {
                    risk: RiskLevel::WriteOutsideWorkspace,
                    policy: types::ExecutionPolicy::AlwaysAsk,
                    reasons: vec![format!("Unknown tool: {}", parsed.tool_id)],
                });
            let description = self
                .tools
                .describe_tool(&tool_id, parsed.args.clone())
                .await
                .unwrap_or_else(|_| format!("Unknown tool: {}", parsed.tool_id));
            let requires_confirmation = !assessment.is_auto_approved(default_autonomy_threshold());

            let call = ToolCall {
                call_id: call_id.clone(),
                tool_id,
                args: parsed.args,
            };

            pending_tool_calls.push((
                call_id.clone(),
                PendingToolCall {
                    call,
                    description: description.clone(),
                    assessment: assessment.clone(),
                    config: active_tool_config.clone(),
                    status: if requires_confirmation {
                        PermissionStatus::Pending
                    } else {
                        PermissionStatus::Approved
                    },
                },
            ));

            detected_calls.push(DetectedToolCall {
                call_id,
                tool_id: parsed.tool_id,
                description,
                assessment,
                requires_confirmation,
            });
        }

        self.ensure_runtime_state(session_id, &session.workspace_dir)
            .pending_tool_calls
            .extend(pending_tool_calls);

        let failed_calls: Vec<(String, String)> = result
            .failed
            .into_iter()
            .map(|failure| (failure.raw_content, failure.error))
            .collect();

        Ok((detected_calls, failed_calls))
    }

    pub async fn add_failed_tool_calls_to_history(
        &mut self,
        session_id: &str,
        failed: Vec<(String, String)>,
    ) -> Result<()> {
        for (raw_content, error) in failed {
            let content = format!(
                "Failed to parse tool call:\n```\n{}\n```\nError: {}",
                raw_content, error
            );
            self.add_message(session_id, ChatRole::Tool, content)
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
            .parse_tool_calls_from_content(session_id, &message.content)
            .await?;

        if !failed.is_empty() {
            self.add_failed_tool_calls_to_history(session_id, failed)
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
            })
            .collect();
        tools.sort_by(|left, right| left.call_id.cmp(&right.call_id));
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

            match pending.status {
                PermissionStatus::Pending => {}
                PermissionStatus::Approved => executions.push(ToolExecutionRequest {
                    call_id,
                    tool_id: pending.call.tool_id,
                    args: Some(pending.call.args),
                    config: Some(pending.config),
                    permission_denied: false,
                }),
                PermissionStatus::Denied => executions.push(ToolExecutionRequest {
                    call_id,
                    tool_id: pending.call.tool_id,
                    args: None,
                    config: None,
                    permission_denied: true,
                }),
            }
        }

        executions
    }

    pub async fn add_tool_results_to_history(
        &mut self,
        session_id: &str,
        results: Vec<ToolResult>,
    ) -> Result<()> {
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

            self.add_message(session_id, ChatRole::Tool, content)
                .await?;
        }
        Ok(())
    }

    pub fn has_pending_tools(&self, session_id: &str) -> bool {
        self.session_states
            .get(session_id)
            .is_some_and(|state| !state.pending_tool_calls.is_empty())
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

fn default_autonomy_threshold() -> RiskLevel {
    RiskLevel::ReadOnlyWorkspace
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::eyre::Result;
    use futures::stream::BoxStream;
    use provider_core::Provider;
    use std::time::{SystemTime, UNIX_EPOCH};
    use types::{ChatMessage as ProviderChatMessage, ExecutionPolicy, MessageStatus};

    struct MockProvider {
        id: ProviderId,
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

        let manager = AgentManager::new(
            providers,
            ToolManager::new(),
            PathBuf::from("/tmp/default-workspace"),
            message_store,
            session_store,
        );
        (manager, data_dir)
    }

    async fn cleanup_dir(data_dir: PathBuf) {
        let _ = tokio::fs::remove_dir_all(data_dir).await;
    }

    #[tokio::test]
    async fn create_session_returns_usable_session_id() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let session_id = manager.create_session().await?;
        let sessions = manager.list_sessions().await?;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session_id);
        assert_eq!(manager.get_tip(&session_id).await?, None);

        cleanup_dir(data_dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn sessions_keep_independent_tips_and_histories() -> Result<()> {
        let (mut manager, data_dir) = test_manager().await;

        let session_a = manager.create_session().await?;
        let session_b = manager.create_session().await?;

        let a_message = manager
            .add_message(&session_a, ChatRole::User, String::from("hello a"))
            .await?;
        let b_message = manager
            .add_message(&session_b, ChatRole::User, String::from("hello b"))
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
    async fn start_stream_failure_rolls_tip_back_to_last_durable_message() -> Result<()> {
        let data_dir = test_dir("stream-failure");
        tokio::fs::create_dir_all(&data_dir).await.unwrap();

        let message_store = Arc::new(persistence::FileMessageStore::new(&data_dir));
        let session_store = Arc::new(persistence::FileSessionStore::new(
            &data_dir,
            message_store.clone(),
        ));
        let manager_providers = ProviderManager::new();
        let mut manager = AgentManager::new(
            manager_providers,
            ToolManager::new(),
            PathBuf::from("/tmp/default-workspace"),
            message_store,
            session_store,
        );

        let session_id = manager.create_session().await?;
        manager
            .add_message(&session_id, ChatRole::User, String::from("hello"))
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
            )
            .await;
        assert!(result.is_err());
        manager.abort_streaming_message(&request.message_id).await?;

        let tip = manager.get_tip(&session_id).await?;
        let history = manager.get_chat_history(&session_id).await?;
        let latest_user_message = history
            .values()
            .filter(|message| message.role == ChatRole::User)
            .max_by_key(|message| message.id.to_string())
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
            .add_message(&session_id, ChatRole::User, String::from("before stream"))
            .await?;
        let streaming_id = manager
            .start_streaming_message(&session_id, ChatRole::Assistant, CallId::new("call-1"))
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
                        tool_id: ToolId::new("mock_tool"),
                        args: serde_json::json!({}),
                    },
                    description: String::from("test"),
                    assessment: ToolCallAssessment {
                        risk: RiskLevel::ReadOnlyWorkspace,
                        policy: ExecutionPolicy::AlwaysAsk,
                        reasons: Vec::new(),
                    },
                    config: types::ToolCallGlobalConfig {
                        workspace_dir: PathBuf::from("/tmp/workspace-a"),
                    },
                    status: PermissionStatus::Pending,
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
}
