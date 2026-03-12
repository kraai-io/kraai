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
  pub agent_profile_id: Option<String>,
}

impl From<TypesMessage> for Message {
  fn from(msg: TypesMessage) -> Self {
    Message {
      id: msg.id.to_string(),
      parent_id: msg.parent_id.map(|id| id.to_string()),
      role: msg.role.into(),
      content: msg.content,
      status: msg.status.into(),
      agent_profile_id: msg.agent_profile_id,
    }
  }
}

#[napi(string_enum)]
#[derive(Clone, Debug)]
pub enum AgentProfileSource {
  BuiltIn,
  Global,
  Workspace,
}

impl From<agent_runtime::AgentProfileSource> for AgentProfileSource {
  fn from(value: agent_runtime::AgentProfileSource) -> Self {
    match value {
      agent_runtime::AgentProfileSource::BuiltIn => AgentProfileSource::BuiltIn,
      agent_runtime::AgentProfileSource::Global => AgentProfileSource::Global,
      agent_runtime::AgentProfileSource::Workspace => AgentProfileSource::Workspace,
    }
  }
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct AgentProfileSummary {
  pub id: String,
  pub display_name: String,
  pub description: String,
  pub tools: Vec<String>,
  pub default_risk_level: String,
  pub source: AgentProfileSource,
}

impl From<agent_runtime::AgentProfileSummary> for AgentProfileSummary {
  fn from(value: agent_runtime::AgentProfileSummary) -> Self {
    AgentProfileSummary {
      id: value.id,
      display_name: value.display_name,
      description: value.description,
      tools: value.tools,
      default_risk_level: value.default_risk_level.as_str().to_string(),
      source: value.source.into(),
    }
  }
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct AgentProfileWarning {
  pub source: AgentProfileSource,
  pub path: Option<String>,
  pub message: String,
}

impl From<agent_runtime::AgentProfileWarning> for AgentProfileWarning {
  fn from(value: agent_runtime::AgentProfileWarning) -> Self {
    AgentProfileWarning {
      source: value.source.into(),
      path: value.path,
      message: value.message,
    }
  }
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct AgentProfilesState {
  pub profiles: Vec<AgentProfileSummary>,
  pub warnings: Vec<AgentProfileWarning>,
  pub selected_profile_id: Option<String>,
  pub profile_locked: bool,
}

impl From<agent_runtime::AgentProfilesState> for AgentProfilesState {
  fn from(value: agent_runtime::AgentProfilesState) -> Self {
    AgentProfilesState {
      profiles: value.profiles.into_iter().map(Into::into).collect(),
      warnings: value.warnings.into_iter().map(Into::into).collect(),
      selected_profile_id: value.selected_profile_id,
      profile_locked: value.profile_locked,
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
  pub workspace_dir: String,
  pub created_at: f64,
  pub updated_at: f64,
  pub title: Option<String>,
  pub selected_profile_id: Option<String>,
  pub profile_locked: bool,
  pub waiting_for_approval: bool,
  pub is_streaming: bool,
}

impl From<SessionMeta> for Session {
  fn from(meta: SessionMeta) -> Self {
    Session {
      id: meta.id,
      tip_id: meta.tip_id.map(|id| id.to_string()),
      workspace_dir: meta.workspace_dir.display().to_string(),
      created_at: meta.created_at as f64,
      updated_at: meta.updated_at as f64,
      title: meta.title,
      selected_profile_id: meta.selected_profile_id,
      profile_locked: false,
      waiting_for_approval: false,
      is_streaming: false,
    }
  }
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct WorkspaceState {
  pub workspace_dir: String,
  pub applies_next_chat: bool,
}

impl From<agent_runtime::WorkspaceState> for WorkspaceState {
  fn from(value: agent_runtime::WorkspaceState) -> Self {
    WorkspaceState {
      workspace_dir: value.workspace_dir,
      applies_next_chat: value.applies_next_chat,
    }
  }
}

#[napi(string_enum)]
#[derive(Clone, Debug)]
pub enum FieldValueKind {
  String,
  SecretString,
  Boolean,
  Integer,
  Url,
}

impl From<agent_runtime::FieldValueKind> for FieldValueKind {
  fn from(value: agent_runtime::FieldValueKind) -> Self {
    match value {
      agent_runtime::FieldValueKind::String => FieldValueKind::String,
      agent_runtime::FieldValueKind::SecretString => FieldValueKind::SecretString,
      agent_runtime::FieldValueKind::Boolean => FieldValueKind::Boolean,
      agent_runtime::FieldValueKind::Integer => FieldValueKind::Integer,
      agent_runtime::FieldValueKind::Url => FieldValueKind::Url,
    }
  }
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct FieldValueEntry {
  pub key: String,
  pub string_value: Option<String>,
  pub bool_value: Option<bool>,
  pub int_value: Option<i64>,
}

impl From<agent_runtime::FieldValueEntry> for FieldValueEntry {
  fn from(value: agent_runtime::FieldValueEntry) -> Self {
    let mut entry = FieldValueEntry {
      key: value.key,
      string_value: None,
      bool_value: None,
      int_value: None,
    };
    match value.value {
      agent_runtime::SettingsValue::String(inner) => entry.string_value = Some(inner),
      agent_runtime::SettingsValue::Bool(inner) => entry.bool_value = Some(inner),
      agent_runtime::SettingsValue::Integer(inner) => entry.int_value = Some(inner),
    }
    entry
  }
}

impl From<FieldValueEntry> for agent_runtime::FieldValueEntry {
  fn from(value: FieldValueEntry) -> Self {
    let converted = if let Some(inner) = value.string_value {
      agent_runtime::SettingsValue::String(inner)
    } else if let Some(inner) = value.bool_value {
      agent_runtime::SettingsValue::Bool(inner)
    } else if let Some(inner) = value.int_value {
      agent_runtime::SettingsValue::Integer(inner)
    } else {
      agent_runtime::SettingsValue::String(String::new())
    };
    agent_runtime::FieldValueEntry {
      key: value.key,
      value: converted,
    }
  }
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct FieldDefinition {
  pub key: String,
  pub label: String,
  pub value_kind: FieldValueKind,
  pub required: bool,
  pub secret: bool,
  pub help_text: Option<String>,
  pub default_string_value: Option<String>,
  pub default_bool_value: Option<bool>,
  pub default_int_value: Option<i64>,
}

impl From<agent_runtime::FieldDefinition> for FieldDefinition {
  fn from(value: agent_runtime::FieldDefinition) -> Self {
    let mut field = FieldDefinition {
      key: value.key,
      label: value.label,
      value_kind: value.value_kind.into(),
      required: value.required,
      secret: value.secret,
      help_text: value.help_text,
      default_string_value: None,
      default_bool_value: None,
      default_int_value: None,
    };
    if let Some(default_value) = value.default_value {
      match default_value {
        agent_runtime::SettingsValue::String(inner) => field.default_string_value = Some(inner),
        agent_runtime::SettingsValue::Bool(inner) => field.default_bool_value = Some(inner),
        agent_runtime::SettingsValue::Integer(inner) => field.default_int_value = Some(inner),
      }
    }
    field
  }
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct ProviderDefinition {
  pub type_id: String,
  pub display_name: String,
  pub protocol_family: String,
  pub description: String,
  pub provider_fields: Vec<FieldDefinition>,
  pub model_fields: Vec<FieldDefinition>,
  pub supports_model_discovery: bool,
  pub default_provider_id_prefix: String,
}

impl From<agent_runtime::ProviderDefinition> for ProviderDefinition {
  fn from(value: agent_runtime::ProviderDefinition) -> Self {
    ProviderDefinition {
      type_id: value.type_id,
      display_name: value.display_name,
      protocol_family: value.protocol_family,
      description: value.description,
      provider_fields: value.provider_fields.into_iter().map(Into::into).collect(),
      model_fields: value.model_fields.into_iter().map(Into::into).collect(),
      supports_model_discovery: value.supports_model_discovery,
      default_provider_id_prefix: value.default_provider_id_prefix,
    }
  }
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct ProviderSettings {
  pub id: String,
  pub type_id: String,
  pub values: Vec<FieldValueEntry>,
}

impl From<agent_runtime::ProviderSettings> for ProviderSettings {
  fn from(value: agent_runtime::ProviderSettings) -> Self {
    ProviderSettings {
      id: value.id,
      type_id: value.type_id,
      values: value.values.into_iter().map(Into::into).collect(),
    }
  }
}

impl From<ProviderSettings> for agent_runtime::ProviderSettings {
  fn from(value: ProviderSettings) -> Self {
    agent_runtime::ProviderSettings {
      id: value.id,
      type_id: value.type_id,
      values: value.values.into_iter().map(Into::into).collect(),
    }
  }
}

/// Model settings exposed to TypeScript.
#[napi(object)]
#[derive(Clone, Debug)]
pub struct ModelSettings {
  pub id: String,
  pub provider_id: String,
  pub values: Vec<FieldValueEntry>,
}

impl From<agent_runtime::ModelSettings> for ModelSettings {
  fn from(value: agent_runtime::ModelSettings) -> Self {
    ModelSettings {
      id: value.id,
      provider_id: value.provider_id,
      values: value.values.into_iter().map(Into::into).collect(),
    }
  }
}

impl From<ModelSettings> for agent_runtime::ModelSettings {
  fn from(value: ModelSettings) -> Self {
    agent_runtime::ModelSettings {
      id: value.id,
      provider_id: value.provider_id,
      values: value.values.into_iter().map(Into::into).collect(),
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
      workspace_dir: s.workspace_dir,
      created_at: s.created_at as f64,
      updated_at: s.updated_at as f64,
      title: s.title,
      selected_profile_id: s.selected_profile_id,
      profile_locked: s.profile_locked,
      waiting_for_approval: s.waiting_for_approval,
      is_streaming: s.is_streaming,
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
    session_id: String,
    message_id: String,
  },
  StreamChunk {
    session_id: String,
    message_id: String,
    chunk: String,
  },
  StreamComplete {
    session_id: String,
    message_id: String,
  },
  StreamError {
    session_id: String,
    message_id: String,
    error: String,
  },
  StreamCancelled {
    session_id: String,
    message_id: String,
  },
  ToolCallDetected {
    session_id: String,
    call_id: String,
    tool_id: String,
    args: String,
    description: String,
    risk_level: String,
    reasons: Vec<String>,
    queue_order: u32,
  },
  ToolResultReady {
    session_id: String,
    call_id: String,
    tool_id: String,
    success: bool,
    output: String,
    denied: bool,
  },
  ContinuationFailed {
    session_id: String,
    error: String,
  },
  HistoryUpdated {
    session_id: String,
  },
}

impl From<agent_runtime::Event> for Event {
  fn from(event: agent_runtime::Event) -> Self {
    match event {
      agent_runtime::Event::ConfigLoaded => Event::ConfigLoaded,
      agent_runtime::Event::Error(e) => Event::Error(e),
      agent_runtime::Event::MessageComplete(id) => Event::MessageComplete(id),
      agent_runtime::Event::StreamStart {
        session_id,
        message_id,
      } => Event::StreamStart {
        session_id,
        message_id,
      },
      agent_runtime::Event::StreamChunk {
        session_id,
        message_id,
        chunk,
      } => Event::StreamChunk {
        session_id,
        message_id,
        chunk,
      },
      agent_runtime::Event::StreamComplete {
        session_id,
        message_id,
      } => Event::StreamComplete {
        session_id,
        message_id,
      },
      agent_runtime::Event::StreamError {
        session_id,
        message_id,
        error,
      } => Event::StreamError {
        session_id,
        message_id,
        error,
      },
      agent_runtime::Event::StreamCancelled {
        session_id,
        message_id,
      } => Event::StreamCancelled {
        session_id,
        message_id,
      },
      agent_runtime::Event::ToolCallDetected {
        session_id,
        call_id,
        tool_id,
        args,
        description,
        risk_level,
        reasons,
        queue_order,
      } => Event::ToolCallDetected {
        session_id,
        call_id,
        tool_id,
        args,
        description,
        risk_level,
        reasons,
        queue_order: queue_order as u32,
      },
      agent_runtime::Event::ToolResultReady {
        session_id,
        call_id,
        tool_id,
        success,
        output,
        denied,
      } => Event::ToolResultReady {
        session_id,
        call_id,
        tool_id,
        success,
        output,
        denied,
      },
      agent_runtime::Event::ContinuationFailed { session_id, error } => {
        Event::ContinuationFailed { session_id, error }
      }
      agent_runtime::Event::HistoryUpdated { session_id } => Event::HistoryUpdated { session_id },
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
  pub async fn list_provider_definitions(&self) -> napi::Result<Vec<ProviderDefinition>> {
    self
      .handle
      .list_provider_definitions()
      .await
      .map(|definitions| definitions.into_iter().map(Into::into).collect())
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn create_session(&self) -> napi::Result<String> {
    self.handle.create_session().await.map_err(to_napi_error)
  }

  #[napi]
  pub async fn send_message(
    &self,
    session_id: String,
    message: String,
    model_id: String,
    provider_id: String,
  ) -> napi::Result<()> {
    self
      .handle
      .send_message(session_id, message, model_id, provider_id)
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
  pub async fn list_agent_profiles(&self, session_id: String) -> napi::Result<AgentProfilesState> {
    self
      .handle
      .list_agent_profiles(session_id)
      .await
      .map(Into::into)
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn set_session_profile(
    &self,
    session_id: String,
    profile_id: String,
  ) -> napi::Result<()> {
    self
      .handle
      .set_session_profile(session_id, profile_id)
      .await
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
    session_id: String,
  ) -> napi::Result<std::collections::BTreeMap<String, Message>> {
    self
      .handle
      .get_chat_history(session_id)
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
  pub async fn get_workspace_state(
    &self,
    session_id: String,
  ) -> napi::Result<Option<WorkspaceState>> {
    self
      .handle
      .get_workspace_state(session_id)
      .await
      .map(|value| value.map(Into::into))
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn set_workspace_dir(
    &self,
    session_id: String,
    workspace_dir: String,
  ) -> napi::Result<()> {
    self
      .handle
      .set_workspace_dir(session_id, workspace_dir)
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn approve_tool(&self, session_id: String, call_id: String) -> napi::Result<()> {
    self
      .handle
      .approve_tool(session_id, call_id)
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn deny_tool(&self, session_id: String, call_id: String) -> napi::Result<()> {
    self
      .handle
      .deny_tool(session_id, call_id)
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn cancel_stream(&self, session_id: String) -> napi::Result<bool> {
    self
      .handle
      .cancel_stream(session_id)
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn execute_approved_tools(&self, session_id: String) -> napi::Result<()> {
    self
      .handle
      .execute_approved_tools(session_id)
      .await
      .map_err(to_napi_error)
  }
}
