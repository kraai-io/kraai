#![deny(clippy::all)]

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use persistence::SessionMeta;
use types::Message as TypesMessage;

// ============================================================================
// NAPI Types - exposed to TypeScript
// ============================================================================

/// Chat role enum exposed to TypeScript
#[napi]
#[derive(Clone, Debug)]
pub enum ChatRole {
  System,
  User,
  Assistant,
  Tool,
}

impl From<types::ChatRole> for ChatRole {
  fn from(role: types::ChatRole) -> Self {
    match role {
      types::ChatRole::System => ChatRole::System,
      types::ChatRole::User => ChatRole::User,
      types::ChatRole::Assistant => ChatRole::Assistant,
      types::ChatRole::Tool => ChatRole::Tool,
    }
  }
}

/// Message status enum exposed to TypeScript
#[napi]
#[derive(Clone, Debug)]
pub enum MessageStatus {
  Complete,
  Streaming { call_id: String },
  ProcessingTools,
  Cancelled,
}

impl From<types::MessageStatus> for MessageStatus {
  fn from(status: types::MessageStatus) -> Self {
    match status {
      types::MessageStatus::Complete => MessageStatus::Complete,
      types::MessageStatus::Streaming { call_id } => MessageStatus::Streaming {
        call_id: call_id.to_string(),
      },
      types::MessageStatus::ProcessingTools => MessageStatus::ProcessingTools,
      types::MessageStatus::Cancelled => MessageStatus::Cancelled,
    }
  }
}

/// Message struct exposed to TypeScript
#[napi(object)]
#[derive(Clone, Debug)]
pub struct Message {
  pub id: String,
  pub parent_id: Option<String>,
  pub role: ChatRole,
  pub content: String,
  pub status: MessageStatus,
}

impl From<TypesMessage> for Message {
  fn from(msg: TypesMessage) -> Self {
    Message {
      id: msg.id.to_string(),
      parent_id: msg.parent_id.map(|id| id.to_string()),
      role: msg.role.into(),
      content: msg.content,
      status: msg.status.into(),
    }
  }
}

/// Model struct exposed to TypeScript
#[napi(object)]
#[derive(Clone, Debug)]
pub struct Model {
  pub id: String,
  pub name: String,
}

impl From<agent_runtime::Model> for Model {
  fn from(m: agent_runtime::Model) -> Self {
    Model {
      id: m.id,
      name: m.name,
    }
  }
}

/// Session struct exposed to TypeScript
#[napi(object)]
#[derive(Clone, Debug)]
pub struct Session {
  pub id: String,
  pub tip_id: Option<String>,
  pub created_at: f64,
  pub updated_at: f64,
  pub title: Option<String>,
}

impl From<SessionMeta> for Session {
  fn from(meta: SessionMeta) -> Self {
    Session {
      id: meta.id,
      tip_id: meta.tip_id.map(|id| id.to_string()),
      created_at: meta.created_at as f64,
      updated_at: meta.updated_at as f64,
      title: meta.title,
    }
  }
}

/// Supported provider types for settings editing.
#[napi(string_enum)]
#[derive(Clone, Debug)]
pub enum ProviderType {
  OpenAi,
  Google,
}

impl From<agent_runtime::ProviderType> for ProviderType {
  fn from(value: agent_runtime::ProviderType) -> Self {
    match value {
      agent_runtime::ProviderType::OpenAi => ProviderType::OpenAi,
      agent_runtime::ProviderType::Google => ProviderType::Google,
    }
  }
}

impl From<ProviderType> for agent_runtime::ProviderType {
  fn from(value: ProviderType) -> Self {
    match value {
      ProviderType::OpenAi => agent_runtime::ProviderType::OpenAi,
      ProviderType::Google => agent_runtime::ProviderType::Google,
    }
  }
}

/// Provider settings exposed to TypeScript.
#[napi(object)]
#[derive(Clone, Debug)]
pub struct ProviderSettings {
  pub id: String,
  pub provider_type: ProviderType,
  pub base_url: Option<String>,
  pub api_key: Option<String>,
  pub env_var_api_key: Option<String>,
  pub only_listed_models: bool,
}

impl From<agent_runtime::ProviderSettings> for ProviderSettings {
  fn from(value: agent_runtime::ProviderSettings) -> Self {
    ProviderSettings {
      id: value.id,
      provider_type: value.provider_type.into(),
      base_url: value.base_url,
      api_key: value.api_key,
      env_var_api_key: value.env_var_api_key,
      only_listed_models: value.only_listed_models,
    }
  }
}

impl From<ProviderSettings> for agent_runtime::ProviderSettings {
  fn from(value: ProviderSettings) -> Self {
    agent_runtime::ProviderSettings {
      id: value.id,
      provider_type: value.provider_type.into(),
      base_url: value.base_url,
      api_key: value.api_key,
      env_var_api_key: value.env_var_api_key,
      only_listed_models: value.only_listed_models,
    }
  }
}

/// Model settings exposed to TypeScript.
#[napi(object)]
#[derive(Clone, Debug)]
pub struct ModelSettings {
  pub id: String,
  pub provider_id: String,
  pub name: Option<String>,
  pub max_context: Option<u32>,
}

impl From<agent_runtime::ModelSettings> for ModelSettings {
  fn from(value: agent_runtime::ModelSettings) -> Self {
    ModelSettings {
      id: value.id,
      provider_id: value.provider_id,
      name: value.name,
      max_context: value.max_context,
    }
  }
}

impl From<ModelSettings> for agent_runtime::ModelSettings {
  fn from(value: ModelSettings) -> Self {
    agent_runtime::ModelSettings {
      id: value.id,
      provider_id: value.provider_id,
      name: value.name,
      max_context: value.max_context,
    }
  }
}

/// Full settings document exposed to TypeScript.
#[napi(object)]
#[derive(Clone, Debug)]
pub struct SettingsDocument {
  pub providers: Vec<ProviderSettings>,
  pub models: Vec<ModelSettings>,
}

impl From<agent_runtime::SettingsDocument> for SettingsDocument {
  fn from(value: agent_runtime::SettingsDocument) -> Self {
    SettingsDocument {
      providers: value.providers.into_iter().map(Into::into).collect(),
      models: value.models.into_iter().map(Into::into).collect(),
    }
  }
}

impl From<SettingsDocument> for agent_runtime::SettingsDocument {
  fn from(value: SettingsDocument) -> Self {
    agent_runtime::SettingsDocument {
      providers: value.providers.into_iter().map(Into::into).collect(),
      models: value.models.into_iter().map(Into::into).collect(),
    }
  }
}

impl From<agent_runtime::Session> for Session {
  fn from(s: agent_runtime::Session) -> Self {
    Session {
      id: s.id,
      tip_id: s.tip_id,
      created_at: s.created_at as f64,
      updated_at: s.updated_at as f64,
      title: s.title,
    }
  }
}

/// Streaming events sent from Rust to TypeScript
#[napi]
#[derive(Clone)]
pub enum Event {
  ConfigLoaded,
  Error(String),
  MessageComplete(String),
  StreamStart {
    message_id: String,
  },
  StreamChunk {
    message_id: String,
    chunk: String,
  },
  StreamComplete {
    message_id: String,
  },
  StreamError {
    message_id: String,
    error: String,
  },
  ToolCallDetected {
    call_id: String,
    tool_id: String,
    args: String,
    description: String,
    risk_level: String,
    reasons: Vec<String>,
  },
  ToolResultReady {
    call_id: String,
    tool_id: String,
    success: bool,
    output: String,
    denied: bool,
  },
  HistoryUpdated,
}

impl From<agent_runtime::Event> for Event {
  fn from(event: agent_runtime::Event) -> Self {
    match event {
      agent_runtime::Event::ConfigLoaded => Event::ConfigLoaded,
      agent_runtime::Event::Error(e) => Event::Error(e),
      agent_runtime::Event::MessageComplete(id) => Event::MessageComplete(id),
      agent_runtime::Event::StreamStart { message_id } => Event::StreamStart { message_id },
      agent_runtime::Event::StreamChunk { message_id, chunk } => {
        Event::StreamChunk { message_id, chunk }
      }
      agent_runtime::Event::StreamComplete { message_id } => Event::StreamComplete { message_id },
      agent_runtime::Event::StreamError { message_id, error } => {
        Event::StreamError { message_id, error }
      }
      agent_runtime::Event::ToolCallDetected {
        call_id,
        tool_id,
        args,
        description,
        risk_level,
        reasons,
      } => Event::ToolCallDetected {
        call_id,
        tool_id,
        args,
        description,
        risk_level,
        reasons,
      },
      agent_runtime::Event::ToolResultReady {
        call_id,
        tool_id,
        success,
        output,
        denied,
      } => Event::ToolResultReady {
        call_id,
        tool_id,
        success,
        output,
        denied,
      },
      agent_runtime::Event::HistoryUpdated => Event::HistoryUpdated,
    }
  }
}

// ============================================================================
// EventCallback Adapter - bridges agent-runtime callback to NAPI
// ============================================================================

/// Adapter that implements EventCallback and forwards to ThreadsafeFunction
struct NapiEventCallback {
  tsfn: ThreadsafeFunction<Event>,
}

impl agent_runtime::EventCallback for NapiEventCallback {
  fn on_event(&self, event: agent_runtime::Event) {
    self
      .tsfn
      .call(Ok(event.into()), ThreadsafeFunctionCallMode::NonBlocking);
  }
}

// ============================================================================
// AgentRuntime - NAPI wrapper around agent-runtime::RuntimeHandle
// ============================================================================

/// The runtime - wraps agent-runtime::RuntimeHandle
#[napi]
pub struct AgentRuntime {
  handle: agent_runtime::RuntimeHandle,
}

fn to_napi_error(err: color_eyre::Report) -> napi::Error {
  napi::Error::new(Status::GenericFailure, format!("{:?}", err))
}

#[napi]
impl AgentRuntime {
  #[napi(constructor)]
  pub fn new(event_callback: ThreadsafeFunction<Event>) -> napi::Result<Self> {
    let callback = NapiEventCallback {
      tsfn: event_callback,
    };
    let handle = agent_runtime::RuntimeBuilder::new(callback).build();
    Ok(AgentRuntime { handle })
  }

  #[napi]
  pub async fn list_models(&self) -> napi::Result<std::collections::HashMap<String, Vec<Model>>> {
    self
      .handle
      .list_models()
      .await
      .map(|map| {
        map
          .into_iter()
          .map(|(provider_id, models)| (provider_id, models.into_iter().map(Into::into).collect()))
          .collect()
      })
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn send_message(
    &self,
    message: String,
    model_id: String,
    provider_id: String,
  ) -> napi::Result<()> {
    self
      .handle
      .send_message(message, model_id, provider_id)
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn get_settings(&self) -> napi::Result<SettingsDocument> {
    self
      .handle
      .get_settings()
      .await
      .map(Into::into)
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn save_settings(&self, settings: SettingsDocument) -> napi::Result<()> {
    self
      .handle
      .save_settings(settings.into())
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn get_chat_history_tree(
    &self,
  ) -> napi::Result<std::collections::BTreeMap<String, Message>> {
    self
      .handle
      .get_chat_history()
      .await
      .map(|history| {
        history
          .into_iter()
          .map(|(id, m)| (id.to_string(), m.into()))
          .collect()
      })
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn clear_current_session(&self) -> napi::Result<()> {
    self
      .handle
      .clear_current_session()
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn load_session(&self, session_id: String) -> napi::Result<bool> {
    self
      .handle
      .load_session(session_id)
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn list_sessions(&self) -> napi::Result<Vec<Session>> {
    self
      .handle
      .list_sessions()
      .await
      .map(|sessions| sessions.into_iter().map(Into::into).collect())
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn delete_session(&self, session_id: String) -> napi::Result<()> {
    self
      .handle
      .delete_session(session_id)
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn get_current_session_id(&self) -> napi::Result<Option<String>> {
    self
      .handle
      .get_current_session_id()
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn approve_tool(&self, call_id: String) -> napi::Result<()> {
    self
      .handle
      .approve_tool(call_id)
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn deny_tool(&self, call_id: String) -> napi::Result<()> {
    self.handle.deny_tool(call_id).await.map_err(to_napi_error)
  }

  #[napi]
  pub async fn execute_approved_tools(&self) -> napi::Result<()> {
    self
      .handle
      .execute_approved_tools()
      .await
      .map_err(to_napi_error)
  }
}
