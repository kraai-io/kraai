use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

use color_eyre::eyre::Result;
use futures::stream::BoxStream;
use persistence::{MessageStore, SessionMeta, SessionStore};
use provider_core::{Model, ProviderManager, ProviderManagerConfig, ProviderManagerHelper};
use tokio::sync::RwLock;
use tool_core::{ToolManager, ToolOutput, toon_parser};
use types::{
    CallId, ChatMessage, ChatRole, Message, MessageId, MessageStatus, ModelId, ProviderId,
    ToolCall, ToolId, ToolResult,
};
use ulid::Ulid;

#[derive(Clone, Debug)]
pub struct PendingToolCall {
    pub call: ToolCall,
    pub description: String,
    pub status: PermissionStatus,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PermissionStatus {
    Pending,
    Approved,
    Denied,
}

pub struct AgentManager {
    providers: ProviderManager,
    tools: ToolManager,
    message_store: Arc<dyn MessageStore>,
    session_store: Arc<dyn SessionStore>,
    current_session_id: Option<String>,
    /// Messages currently being streamed (not yet persisted)
    streaming_messages: RwLock<HashMap<MessageId, Message>>,
    pending_tool_calls: HashMap<CallId, PendingToolCall>,
    last_model: Option<ModelId>,
    last_provider: Option<ProviderId>,
}

impl AgentManager {
    pub fn new(
        providers: ProviderManager,
        tools: ToolManager,
        message_store: Arc<dyn MessageStore>,
        session_store: Arc<dyn SessionStore>,
    ) -> Self {
        Self {
            providers,
            tools,
            message_store,
            session_store,
            current_session_id: None,
            streaming_messages: RwLock::new(HashMap::new()),
            pending_tool_calls: HashMap::new(),
            last_model: None,
            last_provider: None,
        }
    }

    /// Create a new session and set it as current
    pub async fn new_session(&mut self) -> Result<String> {
        self.ensure_session().await
    }

    /// Clear the current session (for starting fresh - session created on first message)
    pub async fn clear_current_session(&mut self) {
        self.current_session_id = None;
        self.pending_tool_calls.clear();
        self.last_model = None;
        self.last_provider = None;
        // Clear any orphaned streaming messages
        self.streaming_messages.write().await.clear();
    }

    /// Load an existing session as current
    pub async fn load_session(&mut self, session_id: &str) -> Result<bool> {
        match self.session_store.get(session_id).await? {
            Some(session) => {
                // Clean up streaming messages from previous session
                self.streaming_messages.write().await.clear();

                // Clean up hot cache for other sessions
                self.cleanup_hot_cache_for_session(&session).await?;

                self.current_session_id = Some(session_id.to_string());
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Clean up hot cache, keeping only messages from the given session's tree
    async fn cleanup_hot_cache_for_session(&self, session: &SessionMeta) -> Result<()> {
        // Collect message IDs that should stay in hot cache
        let mut keep_ids = std::collections::HashSet::new();

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

        // Unload messages not in the keep set
        let hot_ids = self.message_store.list_hot().await?;
        for id in hot_ids.difference(&keep_ids) {
            self.message_store.unload(id).await;
        }

        Ok(())
    }

    /// List all sessions
    pub async fn list_sessions(&self) -> Result<Vec<SessionMeta>> {
        self.session_store.list().await
    }

    /// Delete a session
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        self.session_store.delete(session_id).await
    }

    /// Get current session ID
    pub fn get_current_session_id(&self) -> Option<&str> {
        self.current_session_id.as_deref()
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

    async fn get_current_tip(&self) -> Result<Option<MessageId>> {
        let session_id = match &self.current_session_id {
            Some(id) => id,
            None => return Ok(None),
        };

        match self.session_store.get(session_id).await? {
            Some(session) => Ok(session.tip_id),
            None => Ok(None),
        }
    }

    async fn update_tip(&self, new_tip: MessageId) -> Result<()> {
        let session_id = match &self.current_session_id {
            Some(id) => id.clone(),
            None => return Ok(()),
        };

        if let Some(mut session) = self.session_store.get(&session_id).await? {
            session.tip_id = Some(new_tip);
            session.updated_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or(std::time::Duration::ZERO)
                .as_secs();
            self.session_store.save(&session).await?;
        }

        Ok(())
    }

    pub async fn start_stream(
        &mut self,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> Result<(MessageId, BoxStream<'static, Result<String>>)> {
        self.last_model = Some(model_id.clone());
        self.last_provider = Some(provider_id.clone());

        let user_msg_id = self.add_message(ChatRole::User, message).await?;

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
            .start_streaming_message(ChatRole::Assistant, call_id)
            .await?;

        let stream = self
            .providers
            .generate_reply_stream(provider_id, &model_id, provider_messages)
            .await?;

        Ok((assistant_msg_id, stream))
    }

    pub async fn start_continuation_stream(
        &mut self,
    ) -> Result<Option<(MessageId, BoxStream<'static, Result<String>>)>> {
        let model_id = match &self.last_model {
            Some(m) => m.clone(),
            None => return Ok(None),
        };
        let provider_id = match &self.last_provider {
            Some(p) => p.clone(),
            None => return Ok(None),
        };

        let tip_id = match self.get_current_tip().await? {
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
            .start_streaming_message(ChatRole::Assistant, call_id)
            .await?;

        let stream = self
            .providers
            .generate_reply_stream(provider_id, &model_id, provider_messages)
            .await?;

        Ok(Some((assistant_msg_id, stream)))
    }

    async fn ensure_session(&mut self) -> Result<String> {
        if let Some(id) = &self.current_session_id {
            return Ok(id.clone());
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_secs();

        let session_id = Ulid::new().to_string();
        let session = SessionMeta {
            id: session_id.clone(),
            tip_id: None,
            created_at: now,
            updated_at: now,
            title: None,
        };

        self.session_store.save(&session).await?;
        self.current_session_id = Some(session_id.clone());

        Ok(session_id)
    }

    async fn add_message(&mut self, role: ChatRole, content: String) -> Result<MessageId> {
        // Ensure we have a session before adding messages
        self.ensure_session().await?;

        let message_id = MessageId::new(Ulid::new());
        let parent_id = self.get_current_tip().await?;

        println!(
            "[AGENT] Adding message: id={}, role={:?}, parent={:?}",
            message_id, role, parent_id
        );

        let message = Message {
            id: message_id.clone(),
            parent_id,
            role,
            content,
            status: MessageStatus::Complete,
        };

        self.message_store.save(&message).await?;
        self.update_tip(message_id.clone()).await?;

        Ok(message_id)
    }

    async fn start_streaming_message(
        &mut self,
        role: ChatRole,
        call_id: CallId,
    ) -> Result<MessageId> {
        // Ensure we have a session before adding messages
        self.ensure_session().await?;

        let message_id = MessageId::new(Ulid::new());
        let parent_id = self.get_current_tip().await?;

        let message = Message {
            id: message_id.clone(),
            parent_id,
            role,
            content: String::new(),
            status: MessageStatus::Streaming { call_id },
        };

        // Store in streaming buffer, not persisted yet
        let mut streaming = self.streaming_messages.write().await;
        streaming.insert(message_id.clone(), message);

        // Update tip to point to this streaming message
        self.update_tip(message_id.clone()).await?;

        Ok(message_id)
    }

    pub async fn append_chunk(&self, message_id: &MessageId, chunk: &str) {
        let mut streaming = self.streaming_messages.write().await;
        if let Some(msg) = streaming.get_mut(message_id) {
            msg.content.push_str(chunk);
        }
    }

    pub async fn complete_message(&self, message_id: &MessageId) -> Result<()> {
        let mut streaming = self.streaming_messages.write().await;
        if let Some(mut msg) = streaming.remove(message_id) {
            msg.status = MessageStatus::Complete;
            self.message_store.save(&msg).await?;
        }
        Ok(())
    }

    async fn get_history_context(&self, from: &MessageId) -> Result<Vec<Message>> {
        let mut context = Vec::new();
        let mut current = Some(from.clone());

        while let Some(id) = current {
            // Check streaming buffer first
            {
                let streaming = self.streaming_messages.read().await;
                if let Some(msg) = streaming.get(&id) {
                    context.push(msg.clone());
                    current = msg.parent_id.clone();
                    continue;
                }
            }

            // Then check persistent store
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

    pub async fn get_chat_history(&self) -> Result<BTreeMap<MessageId, Message>> {
        let mut result = BTreeMap::new();

        // Get all messages from the current session's tree
        let tip_id = match self.get_current_tip().await? {
            Some(id) => id,
            None => return Ok(result),
        };

        let context = self.get_history_context(&tip_id).await?;
        for msg in context {
            result.insert(msg.id.clone(), msg);
        }

        // Also include streaming messages
        {
            let streaming = self.streaming_messages.read().await;
            for (id, msg) in streaming.iter() {
                result.insert(id.clone(), msg.clone());
            }
        }

        Ok(result)
    }

    pub async fn parse_tool_calls_from_content(
        &mut self,
        content: &str,
    ) -> (Vec<(CallId, String, String)>, Vec<(String, String)>) {
        let result = toon_parser::parse_tool_calls(content);

        let mut detected_calls = Vec::new();
        for parsed in result.successful {
            let call_id = CallId::new(Ulid::new());
            let tool_id = ToolId::new(&parsed.tool_id);
            let description = self
                .tools
                .describe_tool(&tool_id, parsed.args.clone())
                .await
                .unwrap_or_else(|_| format!("Unknown tool: {}", parsed.tool_id));

            let call = ToolCall {
                call_id: call_id.clone(),
                tool_id,
                args: parsed.args,
            };

            self.pending_tool_calls.insert(
                call_id.clone(),
                PendingToolCall {
                    call,
                    description: description.clone(),
                    status: PermissionStatus::Pending,
                },
            );

            detected_calls.push((call_id, parsed.tool_id, description));
        }

        let failed_calls: Vec<(String, String)> = result
            .failed
            .into_iter()
            .map(|f| (f.raw_content, f.error))
            .collect();

        (detected_calls, failed_calls)
    }

    pub async fn add_failed_tool_calls_to_history(
        &mut self,
        failed: Vec<(String, String)>,
    ) -> Result<()> {
        for (raw_content, error) in failed {
            let content = format!(
                "Failed to parse tool call:\n```\n{}\n```\nError: {}",
                raw_content, error
            );
            self.add_message(ChatRole::Tool, content).await?;
        }
        Ok(())
    }

    pub async fn process_message_for_tools(
        &mut self,
        message_id: &MessageId,
    ) -> Result<Vec<(CallId, String, String)>> {
        let history = self.get_chat_history().await?;
        let message = match history.get(message_id) {
            Some(m) => m,
            None => return Ok(vec![]),
        };

        let (detected, failed) = self.parse_tool_calls_from_content(&message.content).await;

        if !failed.is_empty() {
            self.add_failed_tool_calls_to_history(failed).await?;
        }

        Ok(detected)
    }

    pub fn approve_tool(&mut self, call_id: CallId) -> bool {
        if let Some(pending) = self.pending_tool_calls.get_mut(&call_id) {
            pending.status = PermissionStatus::Approved;
            true
        } else {
            false
        }
    }

    pub fn deny_tool(&mut self, call_id: CallId) -> bool {
        if let Some(pending) = self.pending_tool_calls.get_mut(&call_id) {
            pending.status = PermissionStatus::Denied;
            true
        } else {
            false
        }
    }

    pub async fn execute_approved_tools(&mut self) -> Vec<ToolResult> {
        let pending = std::mem::take(&mut self.pending_tool_calls);

        let approved: Vec<_> = pending
            .iter()
            .filter(|(_, p)| p.status == PermissionStatus::Approved)
            .map(|(call_id, p)| (call_id.clone(), p.call.tool_id.clone(), p.call.args.clone()))
            .collect();

        let mut results = Vec::new();
        for (call_id, tool_id, args) in approved {
            let result = self.tools.call_tool(&tool_id, args).await;
            let output = match result {
                Ok(ToolOutput::Success { data }) => data,
                Ok(ToolOutput::Error { message }) => {
                    serde_json::json!({ "error": message })
                }
                Err(e) => serde_json::json!({ "error": e.to_string() }),
            };
            results.push(ToolResult {
                call_id,
                tool_id,
                output,
                permission_denied: false,
            });
        }

        for (call_id, pending) in pending.iter() {
            if pending.status == PermissionStatus::Denied {
                results.push(ToolResult {
                    call_id: call_id.clone(),
                    tool_id: pending.call.tool_id.clone(),
                    output: serde_json::json!({ "error": "Permission denied by user" }),
                    permission_denied: true,
                });
            }
        }

        results
    }

    pub async fn add_tool_results_to_history(&mut self, results: Vec<ToolResult>) -> Result<()> {
        for result in results {
            let content = if result.permission_denied {
                format!("Tool '{}' was denied by user", result.tool_id)
            } else {
                let output_str = serde_json::to_string_pretty(&result.output)
                    .unwrap_or_else(|_| "{}".to_string());
                format!("Tool '{}' result:\n{}", result.tool_id, output_str)
            };

            println!(
                "[AGENT] Adding tool result to history: tool_id={}, denied={}",
                result.tool_id, result.permission_denied
            );

            self.add_message(ChatRole::Tool, content).await?;
        }
        Ok(())
    }

    pub fn has_pending_tools(&self) -> bool {
        !self.pending_tool_calls.is_empty()
    }

    pub fn get_pending_tool_args(&self, call_id: &CallId) -> Option<serde_json::Value> {
        self.pending_tool_calls
            .get(call_id)
            .map(|p| p.call.args.clone())
    }
}
