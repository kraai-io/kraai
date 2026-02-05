#![deny(clippy::all)]

use std::path::PathBuf;
use std::sync::Arc;

use agent::AgentManager;
use color_eyre::eyre::{Context, Result};
use napi::bindgen_prelude::*;
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;
use provider_core::{ProviderManager, ProviderManagerHelper};
use provider_google::GoogleFactory;
use provider_openai::OpenAIFactory;
use tokio::sync::Mutex;
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

// File operation result types
type FileReadResult = Either<FileError, Uint8Array>;
type FileWriteResult = Either<FileError, ()>;
type ListDirResult = Either<FileError, Vec<String>>;

// AgentAPI class - uses Arc<Mutex<>> for interior mutability
// File access is done through callbacks to TypeScript
#[napi]
pub struct AgentAPI {
  config_dir: Arc<Mutex<PathBuf>>,
  manager: Arc<Mutex<AgentManager>>,
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
      config_dir: Arc::new(Mutex::new("".into())),
      manager: Arc::new(Mutex::new(AgentManager::new(providers, tools))),
      read_file_callback: Arc::new(read_file_callback),
      write_file_callback: Arc::new(write_file_callback),
      list_dir_callback: Arc::new(list_dir_callback),
    })
  }

  #[napi]
  pub async fn reload_config(
    &self,
    config_data: Uint8Array,
    config_dir: String,
  ) -> napi::Result<()> {
    self
      .reload_config_inner(config_data, config_dir)
      .await
      .map_err(to_napi_error)
  }

  async fn reload_config_inner(&self, config_data: Uint8Array, config_dir: String) -> Result<()> {
    {
      let mut config_dir_lock = self.config_dir.lock().await;
      *config_dir_lock = config_dir.into();
    }

    // Parse config data
    let config: provider_core::ProviderManagerConfig =
      toml::from_slice(&config_data).wrap_err("Failed to parse config.toml")?;

    // Load config into provider manager
    // Set up provider factories inside the async block to avoid Send issues
    self
      .load_providers_with_config(config)
      .await
      .wrap_err("Failed to load providers from config")?;

    Ok(())
  }

  async fn load_providers_with_config(
    &self,
    config: provider_core::ProviderManagerConfig,
  ) -> Result<()> {
    // Set up provider factories
    let mut helper = ProviderManagerHelper::default();
    helper.register_factory::<GoogleFactory>();
    helper.register_factory::<OpenAIFactory>();

    // Load config into provider manager
    {
      let mut manager = self.manager.blocking_lock();
      manager.set_providers(config, helper).await?;
    }

    Ok(())
  }

  // TODO: Re-enable when ProviderManager is set up
  // #[napi]
  // pub fn list_models(&self) -> napi::Result<Vec<ModelInfo>> {
  //   Ok(vec![])
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
  // ) -> napi::Result<ChatMessage> {
  //   // TODO: Implement streaming with proper Send bounds
  //   // This requires making ProviderManager Send or restructuring the code
  //   unimplemented!("Streaming not yet implemented due to Send requirements")
  // }

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
