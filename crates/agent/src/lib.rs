use std::collections::BTreeMap;
use types::CallId;
use types::MessageId;
use types::ModelId;
use types::ProviderId;
use types::ToolCall;
use types::ToolId;
use types::ToolResult;

use color_eyre::eyre::Result;
use futures::stream::BoxStream;
use provider_core::{Model, ProviderManager, ProviderManagerConfig, ProviderManagerHelper};
use std::collections::HashMap;
use tokio::sync::RwLock;
use tool_core::{ToolManager, ToolOutput, toon_parser};
use types::{ChatMessage, ChatRole, Message, MessageStatus};
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
    current_session: Session,
    pending_tool_calls: HashMap<CallId, PendingToolCall>,
    last_model: Option<ModelId>,
    last_provider: Option<ProviderId>,
}

impl AgentManager {
    pub fn new(providers: ProviderManager, tools: ToolManager) -> Self {
        Self {
            providers,
            tools,
            current_session: Session::new(),
            pending_tool_calls: HashMap::new(),
            last_model: None,
            last_provider: None,
        }
    }

    pub fn new_session(&mut self) {
        self.current_session = Session::new();
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

    pub async fn start_stream(
        &mut self,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> Result<(MessageId, BoxStream<'static, Result<String>>)> {
        self.last_model = Some(model_id.clone());
        self.last_provider = Some(provider_id.clone());

        let user_msg_id = self
            .current_session
            .add_message(ChatRole::User, message)
            .await;

        let context = self.current_session.get_history_context(&user_msg_id).await;

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
            .current_session
            .start_streaming_message(ChatRole::Assistant, call_id)
            .await;

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

        let tip_id = match self.current_session.get_active_tip().await {
            Some(id) => id,
            None => return Ok(None),
        };

        let context = self.current_session.get_history_context(&tip_id).await;

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
            .current_session
            .start_streaming_message(ChatRole::Assistant, call_id)
            .await;

        let stream = self
            .providers
            .generate_reply_stream(provider_id, &model_id, provider_messages)
            .await?;

        Ok(Some((assistant_msg_id, stream)))
    }

    pub async fn append_chunk(&self, message_id: &MessageId, chunk: &str) {
        self.current_session
            .append_to_streaming(message_id, chunk)
            .await;
    }

    pub async fn complete_message(&self, message_id: &MessageId) {
        self.current_session.complete_streaming(message_id).await;
    }

    pub async fn get_chat_history(&self) -> BTreeMap<MessageId, Message> {
        self.current_session.get_all_messages().await
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

    pub async fn add_failed_tool_calls_to_history(&mut self, failed: Vec<(String, String)>) {
        for (raw_content, error) in failed {
            let content = format!(
                "Failed to parse tool call:\n```\n{}\n```\nError: {}",
                raw_content, error
            );
            self.current_session
                .add_message(ChatRole::Tool, content)
                .await;
        }
    }

    pub async fn process_message_for_tools(
        &mut self,
        message_id: &MessageId,
    ) -> Vec<(CallId, String, String)> {
        let messages = self.current_session.get_all_messages().await;
        let message = match messages.get(message_id) {
            Some(m) => m,
            None => return vec![],
        };

        let (detected, failed) = self.parse_tool_calls_from_content(&message.content).await;

        if !failed.is_empty() {
            self.add_failed_tool_calls_to_history(failed).await;
        }

        detected
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

    pub async fn add_tool_results_to_history(&mut self, results: Vec<ToolResult>) {
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

            self.current_session
                .add_message(ChatRole::Tool, content)
                .await;
        }
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

struct Session {
    messages: RwLock<BTreeMap<MessageId, Message>>,
    active_tip: RwLock<Option<MessageId>>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            messages: RwLock::new(BTreeMap::new()),
            active_tip: RwLock::new(None),
        }
    }

    pub async fn add_message(&self, role: ChatRole, content: String) -> MessageId {
        let message_id = MessageId::new(Ulid::new());
        let parent_id = self.active_tip.read().await.clone();

        println!(
            "[SESSION] Adding message: id={}, role={:?}, parent={:?}",
            message_id, role, parent_id
        );

        let message = Message {
            id: message_id.clone(),
            parent_id,
            role,
            content,
            status: MessageStatus::Complete,
        };

        self.messages
            .write()
            .await
            .insert(message_id.clone(), message);
        *self.active_tip.write().await = Some(message_id.clone());

        message_id
    }

    pub async fn start_streaming_message(&self, role: ChatRole, call_id: CallId) -> MessageId {
        let message_id = MessageId::new(Ulid::new());
        let parent_id = self.active_tip.read().await.clone();

        let message = Message {
            id: message_id.clone(),
            parent_id,
            role,
            content: String::new(),
            status: MessageStatus::Streaming { call_id },
        };

        self.messages
            .write()
            .await
            .insert(message_id.clone(), message);
        *self.active_tip.write().await = Some(message_id.clone());

        message_id
    }

    pub async fn append_to_streaming(&self, message_id: &MessageId, chunk: &str) {
        let mut messages = self.messages.write().await;
        if let Some(msg) = messages.get_mut(message_id) {
            msg.content.push_str(chunk);
        }
    }

    pub async fn complete_streaming(&self, message_id: &MessageId) {
        let mut messages = self.messages.write().await;
        if let Some(msg) = messages.get_mut(message_id) {
            msg.status = MessageStatus::Complete;
        }
    }

    pub async fn set_status(&self, message_id: &MessageId, status: MessageStatus) {
        let mut messages = self.messages.write().await;
        if let Some(msg) = messages.get_mut(message_id) {
            msg.status = status;
        }
    }

    pub async fn get_history_context(&self, from: &MessageId) -> Vec<Message> {
        let messages = self.messages.read().await;
        let mut context = Vec::new();
        let mut current = Some(from.clone());

        while let Some(id) = current {
            if let Some(msg) = messages.get(&id) {
                context.push(msg.clone());
                current = msg.parent_id.clone();
            } else {
                break;
            }
        }

        context.reverse();
        context
    }

    pub async fn get_active_tip(&self) -> Option<MessageId> {
        self.active_tip.read().await.clone()
    }

    pub async fn set_active_tip(&self, message_id: MessageId) {
        *self.active_tip.write().await = Some(message_id);
    }

    pub async fn get_all_messages(&self) -> BTreeMap<MessageId, Message> {
        self.messages.read().await.clone()
    }
}
