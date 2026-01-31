use std::{collections::BTreeMap, sync::Arc};

use provider_core::ProviderManager;
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
