use std::{collections::BTreeMap, sync::Arc};
use types::MessageId;
use types::ModelId;
use types::ProviderId;
use types::SessionId;

use color_eyre::eyre::Result;
use provider_core::{Model, ProviderManager, ProviderManagerConfig, ProviderManagerHelper};
use std::collections::HashMap;
use tokio::sync::RwLock;
use tool_core::ToolManager;
use types::ChatMessage;
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
    ) -> Result<ChatMessage> {
        let message_id = self
            .current_session
            .add_message(ChatMessage {
                role: types::ChatRole::User,
                content: message,
            })
            .await;

        let res = self
            .providers
            .generate_reply(
                provider_id,
                &model_id,
                self.current_session.get_history_context(message_id).await,
            )
            .await?;
        self.current_session.add_message(res.clone()).await;
        Ok(res)
    }

    pub async fn get_chat_history(&self) -> BTreeMap<MessageId, ChatMessage> {
        self.current_session.get_history_tree().await
    }
}

struct Session {
    history: RwLock<BTreeMap<MessageId, ChatMessage>>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            history: RwLock::new(BTreeMap::new()),
        }
    }

    pub async fn add_message(&self, message: ChatMessage) -> MessageId {
        let message_id = MessageId::new(Ulid::new());
        self.history
            .write()
            .await
            .insert(message_id.clone(), message);
        message_id
    }

    pub async fn get_history_tree(&self) -> BTreeMap<MessageId, ChatMessage> {
        self.history.read().await.clone()
    }

    pub async fn get_history_context(&self, message_id: MessageId) -> Vec<ChatMessage> {
        let history = self.history.read().await.clone();
        let mut vec = vec![];
        vec.extend(history.into_values());
        vec
    }
}
