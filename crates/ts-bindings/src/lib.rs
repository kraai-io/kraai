#![deny(clippy::all)]

use std::sync::Arc;

use agent::AgentManager;
use color_eyre::eyre::{Context, Result, eyre};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use provider_core::{Model, ProviderManager, ProviderManagerHelper};
use provider_google::GoogleFactory;
use provider_openai::OpenAIFactory;
use tokio::sync::Mutex;
use tool_core::ToolManager;
use types::{ChatMessage as RustChatMessage, ChatRole as RustChatRole};

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
  Tool,
}

impl From<RustChatRole> for ChatRole {
  fn from(role: RustChatRole) -> Self {
    match role {
      RustChatRole::System => ChatRole::System,
      RustChatRole::User => ChatRole::User,
      RustChatRole::Assistant => ChatRole::Assistant,
      RustChatRole::Tool => ChatRole::Tool,
    }
  }
}

impl From<ChatRole> for RustChatRole {
  fn from(role: ChatRole) -> Self {
    match role {
      ChatRole::System => RustChatRole::System,
      ChatRole::User => RustChatRole::User,
      ChatRole::Assistant => RustChatRole::Assistant,
      ChatRole::Tool => RustChatRole::Tool,
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
#[derive(Debug, Clone)]
pub struct ModelInfo {
  pub id: String,
  pub name: String,
  pub max_context: Option<u32>,
}

impl From<Model> for ModelInfo {
  fn from(value: Model) -> Self {
    ModelInfo {
      id: value.id.to_string(),
      name: value.name,
      max_context: value
        .max_context
        .and_then(|x| Some(u32::try_from(x).expect("model context too large"))),
    }
  }
}

#[napi]
pub struct AgentAPI {
  manager: Arc<Mutex<AgentManager>>,
}

#[napi]
impl AgentAPI {
  #[napi(constructor)]
  pub fn new() -> napi::Result<Self> {
    Self::new_inner().map_err(to_napi_error)
  }

  fn new_inner() -> Result<Self> {
    let providers = ProviderManager::new();
    let tools = ToolManager::default();
    let agent_api = AgentAPI {
      manager: Arc::new(Mutex::new(AgentManager::new(providers, tools))),
    };
    Ok(agent_api)
  }

  async fn reload_config(&self) -> Result<()> {
    let config_loc = directories::BaseDirs::new()
      .expect("Failed to find user directories")
      .home_dir()
      .join(".agent-desktop/providers.toml");
    if !config_loc.exists() {
      return Err(eyre!("config file doesnt exist")); // TODO create a default config
    }
    let config_slice = tokio::fs::read(config_loc).await?;
    let config: provider_core::ProviderManagerConfig =
      toml::from_slice(&config_slice).wrap_err("Failed to parse config.toml")?;

    // Load config into provider manager
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
      let mut manager = self.manager.lock().await;
      manager.set_providers(config, helper).await?;
    }

    Ok(())
  }

  #[napi]
  pub fn list_models(&self) -> Vec<ModelInfo> {
    self
      .manager
      .blocking_lock()
      .list_models()
      .into_iter()
      .map(|x| ModelInfo::from(x))
      .collect()
  }
}
