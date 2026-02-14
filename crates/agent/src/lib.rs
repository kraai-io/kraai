use std::{collections::BTreeMap, sync::Arc};
use types::CallId;
use types::MessageId;
use types::ModelId;
use types::ProviderId;
use types::SessionId;

use color_eyre::eyre::Result;
use provider_core::{Model, ProviderManager, ProviderManagerConfig, ProviderManagerHelper};
use std::collections::HashMap;
use tokio::sync::RwLock;
use tool_core::ToolManager;
use types::{ChatMessage, ChatRole, Message, MessageStatus};
use ulid::Ulid;

pub struct AgentManager {
    providers: ProviderManager,
    tools: ToolManager,
    // sessions: RwLock<BTreeMap<SessionId, Arc<Session>>>,
    current_session: Session,
}

impl AgentManager {
    pub fn new(providers: ProviderManager, tools: ToolManager) -> Self {
        Self {
            providers,
            tools,
            // sessions: RwLock::new(BTreeMap::new()),
            current_session: Session::new(),
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

    pub async fn send_message(
        &mut self,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> Result<Message> {
        let user_msg_id = self
            .current_session
            .add_message(ChatRole::User, message)
            .await;

        let context = self.current_session.get_history_context(&user_msg_id).await;

        let provider_messages: Vec<ChatMessage> = context
            .into_iter()
            .map(|m| ChatMessage {
                role: m.role,
                content: m.content,
            })
            .collect();

        let response = self
            .providers
            .generate_reply(provider_id, &model_id, provider_messages)
            .await?;

        let assistant_msg_id = self
            .current_session
            .add_message(response.role, response.content)
            .await;

        let messages = self.current_session.messages.read().await;
        Ok(messages.get(&assistant_msg_id).cloned().unwrap())
    }

    pub async fn get_chat_history(&self) -> BTreeMap<MessageId, Message> {
        self.current_session.get_all_messages().await
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

        let message = Message {
            id: message_id.clone(),
            parent_id,
            role,
            content,
            status: MessageStatus::Complete,
        };

        self.messages.write().await.insert(message_id.clone(), message);
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

        self.messages.write().await.insert(message_id.clone(), message);
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
