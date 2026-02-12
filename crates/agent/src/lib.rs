use std::{collections::BTreeMap, sync::Arc};

use color_eyre::eyre::Result;
use provider_core::{
    Model, ModelId, ProviderId, ProviderManager, ProviderManagerConfig, ProviderManagerHelper,
};
use std::collections::HashMap;
use tool_core::{ToolId, ToolManager};
use types::ChatMessage;

pub struct AgentManager {
    providers: ProviderManager,
    tools: ToolManager,
    current_session: Session,
    // TODO setup multiple sessions
    // sessions: BTreeMap<SessionId, Session>,
}

impl AgentManager {
    pub fn new(providers: ProviderManager, tools: ToolManager) -> Self {
        Self {
            providers,
            tools,
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

    pub fn list_models(&self) -> HashMap<ProviderId, Vec<Model>> {
        self.providers.list_all_models()
    }

    pub async fn send_message(
        &mut self,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> Result<ChatMessage> {
        self.current_session.add_message(ChatMessage {
            role: types::ChatRole::User,
            content: message,
        });

        let res = self
            .providers
            .generate_reply(provider_id, &model_id, self.current_session.get_history())
            .await?;
        self.current_session.add_message(res.clone());
        Ok(res)
    }

    pub fn get_chat_history(&self) -> Vec<ChatMessage> {
        self.current_session.get_history()
    }
}

pub type SessionId = Arc<String>;

pub struct Session {
    history: Vec<ChatMessage>,
}

impl Session {
    pub fn new() -> Self {
        Self { history: vec![] }
    }

    pub fn add_message(&mut self, message: ChatMessage) {
        self.history.push(message);
    }

    pub fn get_history(&self) -> Vec<ChatMessage> {
        self.history.clone()
    }
}
