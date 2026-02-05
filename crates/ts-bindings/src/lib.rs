#![deny(clippy::all)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use agent::AgentManager;
use color_eyre::eyre::{Context, Result};
use napi::bindgen_prelude::*;
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;
use provider_core::{ProviderManager, ProviderManagerHelper};
use provider_google::GoogleFactory;
use provider_openai::OpenAIFactory;
use tool_core::ToolManager;
use types::{ChatMessage as RustChatMessage, ChatRole as RustChatRole};

// Re-export the simple function for testing
#[napi]
pub fn plus_100(input: u32) -> u32 {
  input + 100
}

// Simple HTTP request test
#[napi]
pub async fn test_http_request(url: String) -> napi::Result<String> {
  test_http_request_inner(url).await.map_err(to_napi_error)
}

async fn test_http_request_inner(url: String) -> Result<String> {
  let client = reqwest::Client::new();
  let response = client
    .get(&url)
    .send()
    .await
    .wrap_err_with(|| format!("Failed to send HTTP request to {}", url))?;

  let status = response.status();
  let body = response
    .text()
    .await
    .wrap_err("Failed to read response body")?;

  Ok(format!("Status: {}\nBody: {}", status, body))
}

// File error types for callback-based file operations
#[napi]
#[derive(Clone, Debug)]
pub enum FileError {
  NotFound { path: String },
  PermissionDenied { path: String, operation: String },
  InvalidPath { path: String, reason: String },
  IoError { path: String, message: String },
  UserCancelled,
  ParseError { path: String, message: String },
}

fn to_napi_error(err: color_eyre::Report) -> napi::Error {
  napi::Error::new(Status::GenericFailure, format!("{:?}", err))
}

// Helper to convert Mutex poison errors to eyre errors
fn lock_error<T>(err: PoisonError<T>) -> color_eyre::Report {
  color_eyre::eyre::eyre!("Lock poisoned: {}", err)
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

// File operation result types
type FileReadResult = Either<FileError, Uint8Array>;
type FileWriteResult = Either<FileError, ()>;
type ListDirResult = Either<FileError, Vec<String>>;

// AgentAPI class - uses Arc<Mutex<>> for interior mutability
// File access is done through callbacks to TypeScript
#[napi]
pub struct AgentAPI {
  manager: AgentManager,
  agents: Arc<Mutex<BTreeMap<String, AgentState>>>,
  next_agent_id: Arc<Mutex<u32>>,
  // File operation callbacks (wrapped in Arc for Clone)
  read_file_callback: Arc<ThreadsafeFunction<String, FileReadResult>>,
  write_file_callback: Arc<ThreadsafeFunction<(String, Uint8Array), FileWriteResult>>,
  list_dir_callback: Arc<ThreadsafeFunction<String, ListDirResult>>,
}

#[napi]
impl AgentAPI {
  #[napi(constructor)]
  pub fn new(
    read_file_callback: ThreadsafeFunction<String, Either<FileError, Uint8Array>>,
    write_file_callback: ThreadsafeFunction<(String, Uint8Array), Either<FileError, ()>>,
    list_dir_callback: ThreadsafeFunction<String, Either<FileError, Vec<String>>>,
  ) -> napi::Result<Self> {
    Self::new_inner(read_file_callback, write_file_callback, list_dir_callback)
      .map_err(to_napi_error)
  }

  fn new_inner(
    read_file_callback: ThreadsafeFunction<String, Either<FileError, Uint8Array>>,
    write_file_callback: ThreadsafeFunction<(String, Uint8Array), Either<FileError, ()>>,
    list_dir_callback: ThreadsafeFunction<String, Either<FileError, Vec<String>>>,
  ) -> Result<Self> {
    let providers = ProviderManager::new();
    let tools = ToolManager::default();
    Ok(AgentAPI {
      manager: AgentManager::new(providers, tools),
      agents: Arc::new(Mutex::new(BTreeMap::new())),
      next_agent_id: Arc::new(Mutex::new(0)),
      read_file_callback: Arc::new(read_file_callback),
      write_file_callback: Arc::new(write_file_callback),
      list_dir_callback: Arc::new(list_dir_callback),
    })
  }

  async fn read_config(&mut self) -> Result<()> {
    let mut helper = ProviderManagerHelper::default();
    helper.register_factory::<GoogleFactory>();
    helper.register_factory::<OpenAIFactory>();

    // self.read_file_inner(path).await;
    // let config_slice = std::fs::read("~/config.toml")?;
    let config = toml::from_slice(&config_slice)?;

    self.manager.set_providers(config, helper);
    Ok(())
  }

  // TODO: Re-enable when ProviderManager is set up
  // #[napi]
  // pub fn list_models(&self) -> napi::Result<Vec<ModelInfo>> {
  //   Ok(vec![])
  // }

  #[napi]
  pub fn create_agent(&self, system_prompt: String) -> napi::Result<AgentHandle> {
    self
      .create_agent_inner(system_prompt)
      .map_err(to_napi_error)
  }

  fn create_agent_inner(&self, system_prompt: String) -> Result<AgentHandle> {
    let mut agents = self.agents.lock().map_err(lock_error)?;
    let mut next_id = self.next_agent_id.lock().map_err(lock_error)?;

    let id = format!("agent_{}", *next_id);
    *next_id += 1;

    let agent = AgentState {
      id: id.clone(),
      system_prompt: system_prompt.clone(),
      history: vec![RustChatMessage {
        role: RustChatRole::System,
        content: system_prompt,
      }],
    };

    agents.insert(id.clone(), agent);

    Ok(AgentHandle { id })
  }

  #[napi]
  pub fn get_agent(&self, id: String) -> napi::Result<Option<AgentHandle>> {
    self.get_agent_inner(id).map_err(to_napi_error)
  }

  fn get_agent_inner(&self, id: String) -> Result<Option<AgentHandle>> {
    let agents = self.agents.lock().map_err(lock_error)?;

    if agents.contains_key(&id) {
      Ok(Some(AgentHandle { id }))
    } else {
      Ok(None)
    }
  }

  #[napi]
  pub fn list_agents(&self) -> napi::Result<Vec<AgentInfo>> {
    self.list_agents_inner().map_err(to_napi_error)
  }

  fn list_agents_inner(&self) -> Result<Vec<AgentInfo>> {
    let agents = self.agents.lock().map_err(lock_error)?;

    Ok(
      agents
        .values()
        .map(|a| AgentInfo {
          id: a.id.clone(),
          system_prompt: a.system_prompt.clone(),
          message_count: a.history.len() as u32,
        })
        .collect(),
    )
  }

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
  // ) -> napi::Result<ChatMessage> {
  //   // TODO: Implement streaming with proper Send bounds
  //   // This requires making ProviderManager Send or restructuring the code
  //   unimplemented!("Streaming not yet implemented due to Send requirements")
  // }

  #[napi]
  pub fn get_history(&self, agent_id: String) -> napi::Result<Vec<ChatMessage>> {
    self.get_history_inner(agent_id).map_err(to_napi_error)
  }

  fn get_history_inner(&self, agent_id: String) -> Result<Vec<ChatMessage>> {
    let agents = self.agents.lock().map_err(lock_error)?;

    let agent = agents
      .get(&agent_id)
      .ok_or_else(|| color_eyre::eyre::eyre!("Agent not found: {}", agent_id))?;

    Ok(
      agent
        .history
        .iter()
        .map(|m| ChatMessage::from(m.clone()))
        .collect(),
    )
  }

  // File operation methods
  #[napi]
  pub async fn read_file(&self, path: String) -> napi::Result<Either<FileError, Uint8Array>> {
    self.read_file_inner(path).await.map_err(to_napi_error)
  }

  async fn read_file_inner(&self, path: String) -> Result<Either<FileError, Uint8Array>> {
    self
      .read_file_callback
      .call_async(Ok(path))
      .await
      .wrap_err("Failed to invoke read_file callback")
  }

  #[napi]
  pub async fn write_file(
    &self,
    path: String,
    data: Uint8Array,
  ) -> napi::Result<Either<FileError, ()>> {
    self
      .write_file_inner(path, data)
      .await
      .map_err(to_napi_error)
  }

  async fn write_file_inner(
    &self,
    path: String,
    data: Uint8Array,
  ) -> Result<Either<FileError, ()>> {
    self
      .write_file_callback
      .call_async(Ok((path, data)))
      .await
      .wrap_err("Failed to invoke write_file callback")
  }

  #[napi]
  pub async fn list_dir(&self, path: String) -> napi::Result<Either<FileError, Vec<String>>> {
    self.list_dir_inner(path).await.map_err(to_napi_error)
  }

  async fn list_dir_inner(&self, path: String) -> Result<Either<FileError, Vec<String>>> {
    self
      .list_dir_callback
      .call_async(Ok(path))
      .await
      .wrap_err("Failed to invoke list_dir callback")
  }

  // TODO: Re-enable when ProviderManager is set up
  // #[napi]
  // pub async fn reload_config(&self) -> napi::Result<()> {
  //   Ok(())
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
