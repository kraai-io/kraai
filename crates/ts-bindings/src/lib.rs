#![deny(clippy::all)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use color_eyre::eyre::{Result, eyre};
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use provider_core::{ProviderManager, ProviderManagerConfig};
use provider_google::GoogleFactory;
use provider_openai::OpenAIFactory;
use types::{ChatMessage as RustChatMessage, ChatRole as RustChatRole};

// Re-export the simple function for testing
#[napi]
pub fn plus_100(input: u32) -> u32 {
  input + 100
}

// Enums
#[napi]
#[derive(Clone)]
pub enum ChatRole {
  System,
  User,
  Assistant,
}

impl From<RustChatRole> for ChatRole {
  fn from(role: RustChatRole) -> Self {
    match role {
      RustChatRole::System => ChatRole::System,
      RustChatRole::User => ChatRole::User,
      RustChatRole::Assistant => ChatRole::Assistant,
      RustChatRole::Tool => ChatRole::Assistant, // Map Tool to Assistant for now
    }
  }
}

impl From<ChatRole> for RustChatRole {
  fn from(role: ChatRole) -> Self {
    match role {
      ChatRole::System => RustChatRole::System,
      ChatRole::User => RustChatRole::User,
      ChatRole::Assistant => RustChatRole::Assistant,
    }
  }
}

// Objects
#[napi(object)]
#[derive(Clone)]
pub struct ChatMessage {
  pub role: ChatRole,
  pub content: String,
}

impl From<RustChatMessage> for ChatMessage {
  fn from(msg: RustChatMessage) -> Self {
    ChatMessage {
      role: msg.role.into(),
      content: msg.content,
    }
  }
}

impl From<ChatMessage> for RustChatMessage {
  fn from(msg: ChatMessage) -> Self {
    RustChatMessage {
      role: msg.role.into(),
      content: msg.content,
    }
  }
}

#[napi(object)]
pub struct AgentInfo {
  pub id: String,
  pub system_prompt: String,
  pub message_count: u32,
}

#[napi(object)]
pub struct ModelInfo {
  pub id: String,
  pub name: String,
  pub max_context: Option<u32>,
}

// Internal state managed by AgentAPI
struct AgentState {
  id: String,
  system_prompt: String,
  history: Vec<RustChatMessage>,
}

// AgentAPI class - uses Arc<Mutex<>> for interior mutability
// Mark as Send since we need to use it across async boundaries
#[napi]
pub struct AgentAPI {
  providers: Arc<Mutex<ProviderManager>>,
  agents: Arc<Mutex<BTreeMap<String, AgentState>>>,
  next_agent_id: Arc<Mutex<u32>>,
}

// Manual Send implementation since ProviderManager is not automatically Send
// This is safe because we use Mutex for all access
unsafe impl Send for AgentAPI {}
unsafe impl Sync for AgentAPI {}

#[napi]
impl AgentAPI {
  #[napi(constructor)]
  pub fn new() -> Result<Self> {
    let rt = tokio::runtime::Runtime::new()
      .map_err(|e| eyre!(format!("Failed to create runtime: {}", e),))?;

    let providers = rt.block_on(async {
      let mut providers = ProviderManager::new();
      providers.register_factory::<GoogleFactory>();
      providers.register_factory::<OpenAIFactory>();

      // Load config from file
      let config_slice = match std::fs::read("crates/ts-bindings/config/config.toml") {
        Ok(data) => data,
        Err(e) => {
          return Err(eyre!(format!("Failed to read config: {}", e),));
        }
      };

      let config: ProviderManagerConfig = match toml::from_slice(&config_slice) {
        Ok(cfg) => cfg,
        Err(e) => {
          return Err(eyre!(format!("Failed to parse config: {}", e),));
        }
      };

      if let Err(e) = providers.load_config(config).await {
        return Err(eyre!(format!("Failed to load config: {}", e),));
      }

      Ok(providers)
    })?;

    Ok(AgentAPI {
      providers: Arc::new(Mutex::new(providers)),
      agents: Arc::new(Mutex::new(BTreeMap::new())),
      next_agent_id: Arc::new(Mutex::new(0)),
    })
  }

  // #[napi]
  // pub fn list_models(&self) -> Result<Vec<ModelInfo>> {
  //   let providers = self
  //     .providers
  //     .lock()
  //     .map_err(|e| Error::new(Status::GenericFailure, format!("Lock error: {}", e)))?;

  //   let models = providers.list_all_models();
  //   Ok(
  //     models
  //       .into_iter()
  //       .map(|m| ModelInfo {
  //         id: (*m.id).clone(),
  //         name: m.name,
  //         max_context: m.max_context.map(|n| n as u32),
  //       })
  //       .collect(),
  //   )
  // }

  // #[napi]
  // pub fn create_agent(&self, system_prompt: String) -> Result<AgentHandle> {
  //   let mut agents = self
  //     .agents
  //     .lock()
  //     .map_err(|e| Error::new(Status::GenericFailure, format!("Lock error: {}", e)))?;
  //   let mut next_id = self
  //     .next_agent_id
  //     .lock()
  //     .map_err(|e| Error::new(Status::GenericFailure, format!("Lock error: {}", e)))?;

  //   let id = format!("agent_{}", *next_id);
  //   *next_id += 1;

  //   let agent = AgentState {
  //     id: id.clone(),
  //     system_prompt: system_prompt.clone(),
  //     history: vec![RustChatMessage {
  //       role: RustChatRole::System,
  //       content: system_prompt,
  //     }],
  //   };

  //   agents.insert(id.clone(), agent);

  //   Ok(AgentHandle { id })
  // }

  // #[napi]
  // pub fn get_agent(&self, id: String) -> Result<Option<AgentHandle>> {
  //   let agents = self
  //     .agents
  //     .lock()
  //     .map_err(|e| Error::new(Status::GenericFailure, format!("Lock error: {}", e)))?;

  //   if agents.contains_key(&id) {
  //     Ok(Some(AgentHandle { id }))
  //   } else {
  //     Ok(None)
  //   }
  // }

  // #[napi]
  // pub fn list_agents(&self) -> Result<Vec<AgentInfo>> {
  //   let agents = self
  //     .agents
  //     .lock()
  //     .map_err(|e| Error::new(Status::GenericFailure, format!("Lock error: {}", e)))?;

  //   Ok(
  //     agents
  //       .values()
  //       .map(|a| AgentInfo {
  //         id: a.id.clone(),
  //         system_prompt: a.system_prompt.clone(),
  //         message_count: a.history.len() as u32,
  //       })
  //       .collect(),
  //   )
  // }

  // #[napi]
  // pub async fn send_message(
  //   &self,
  //   agent_id: String,
  //   message: String,
  //   model_id: String,
  //   provider_id: String,
  //   on_token: ThreadsafeFunction<String>,
  //   on_complete: ThreadsafeFunction<ChatMessage>,
  //   on_error: ThreadsafeFunction<String>,
  // ) -> Result<ChatMessage> {
  //   use futures::StreamExt;

  //   // Clone Arcs for use in async block
  //   let agents_arc = self.agents.clone();
  //   let providers_arc = self.providers.clone();

  //   // Add user message to history
  //   {
  //     let mut agents = agents_arc
  //       .lock()
  //       .map_err(|e| Error::new(Status::GenericFailure, format!("Lock error: {}", e)))?;

  //     let agent = agents
  //       .get_mut(&agent_id)
  //       .ok_or_else(|| Error::new(Status::GenericFailure, "Agent not found"))?;

  //     let user_msg = RustChatMessage {
  //       role: RustChatRole::User,
  //       content: message,
  //     };
  //     agent.history.push(user_msg);
  //   }

  //   // Get history and generate response
  //   let history = {
  //     let agents = agents_arc
  //       .lock()
  //       .map_err(|e| Error::new(Status::GenericFailure, format!("Lock error: {}", e)))?;

  //     let agent = agents
  //       .get(&agent_id)
  //       .ok_or_else(|| Error::new(Status::GenericFailure, "Agent not found"))?;

  //     agent.history.clone()
  //   };

  //   let mut providers = providers_arc
  //     .lock()
  //     .map_err(|e| eyre!(format!("Lock error: {}", e)))?;
  //   // Generate streaming response
  //   let mut stream = {
  //     let provider_id_arc = Arc::new(provider_id);
  //     let model_id_arc = Arc::new(model_id);

  //     match providers
  //       .generate_reply_stream(provider_id_arc, &model_id_arc, history)
  //       .await
  //     {
  //       Ok(s) => s,
  //       Err(e) => {
  //         let error_msg = format!("Failed to generate response: {}", e);
  //         on_error.call(Ok(error_msg), ThreadsafeFunctionCallMode::NonBlocking);
  //         return Err(eyre!(e.to_string()));
  //       }
  //     }
  //   };

  //   // Collect streaming response
  //   let mut full_response = String::new();

  //   while let Some(chunk_result) = stream.next().await {
  //     match chunk_result {
  //       Ok(chunk) => {
  //         full_response.push_str(&chunk);
  //         // Stream token to JavaScript
  //         on_token.call(Ok(chunk), ThreadsafeFunctionCallMode::NonBlocking);
  //       }
  //       Err(e) => {
  //         let error_msg = format!("Stream error: {}", e);
  //         on_error.call(Ok(error_msg), ThreadsafeFunctionCallMode::NonBlocking);
  //         return Err(eyre!(e.to_string()));
  //       }
  //     }
  //   }

  //   // Create assistant message and update history
  //   let assistant_msg = RustChatMessage {
  //     role: RustChatRole::Assistant,
  //     content: full_response.clone(),
  //   };

  //   {
  //     let mut agents = agents_arc
  //       .lock()
  //       .map_err(|e| Error::new(Status::GenericFailure, format!("Lock error: {}", e)))?;

  //     let agent = agents
  //       .get_mut(&agent_id)
  //       .ok_or_else(|| Error::new(Status::GenericFailure, "Agent not found"))?;

  //     agent.history.push(assistant_msg.clone());
  //   }

  //   // Convert to JS type and call complete callback
  //   let js_msg = ChatMessage::from(assistant_msg);
  //   on_complete.call(Ok(js_msg.clone()), ThreadsafeFunctionCallMode::NonBlocking);

  //   Ok(js_msg)
  // }

  // #[napi]
  // pub fn get_history(&self, agent_id: String) -> Result<Vec<ChatMessage>> {
  //   let agents = self
  //     .agents
  //     .lock()
  //     .map_err(|e| Error::new(Status::GenericFailure, format!("Lock error: {}", e)))?;

  //   let agent = agents
  //     .get(&agent_id)
  //     .ok_or_else(|| Error::new(Status::GenericFailure, "Agent not found"))?;

  //   Ok(
  //     agent
  //       .history
  //       .iter()
  //       .map(|m| ChatMessage::from(m.clone()))
  //       .collect(),
  //   )
  // }
}

// AgentHandle class
#[napi]
pub struct AgentHandle {
  pub id: String,
}

#[napi]
impl AgentHandle {
  #[napi(getter)]
  pub fn id(&self) -> String {
    self.id.clone()
  }
}
