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
    agents: BTreeMap<AgentId, Agent>,
}

impl AgentManager {
    pub fn new(providers: ProviderManager, tools: ToolManager) -> Self {
        Self {
            providers,
            tools,
            agents: BTreeMap::new(),
        }
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
        let messages = vec![ChatMessage {
            role: types::ChatRole::User,
            content: message,
        }];
        self.providers
            .generate_reply(provider_id, &model_id, messages)
            .await
    }
}

pub type AgentId = Arc<String>;

pub struct Agent {
    history: Vec<ChatMessage>,
    temporary_messages: Vec<ChatMessage>,
    current_tools: Vec<ToolId>,
    dynamic_tools: bool,
}

impl Agent {
    pub fn new(system_prompt: String) -> Self {
        Self {
            history: vec![ChatMessage {
                role: types::ChatRole::System,
                content: system_prompt,
            }],
            temporary_messages: vec![],
            current_tools: vec![],
            dynamic_tools: true,
        }
    }

    pub fn add_message(&mut self, message: ChatMessage) {
        self.history.push(message);
    }

    pub fn add_temporary_message(&mut self, message: ChatMessage) {
        self.temporary_messages.push(message);
    }

    pub fn get_history(&mut self) -> Vec<ChatMessage> {
        let mut messages = self.history.clone();
        messages.append(&mut self.temporary_messages);
        messages
    }
}
