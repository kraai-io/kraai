#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

fn validate_id(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("id cannot be empty".to_string());
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChatRole {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "user")]
    User,
    #[serde(rename = "assistant")]
    Assistant,
    #[serde(rename = "tool")]
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub parent_id: Option<MessageId>,
    pub role: ChatRole,
    pub content: String,
    pub status: MessageStatus,
    #[serde(default)]
    pub agent_profile_id: Option<String>,
    #[serde(default)]
    pub tool_state_snapshot: Option<ToolStateSnapshot>,
    #[serde(default)]
    pub tool_state_deltas: Vec<ToolStateDelta>,
    #[serde(default)]
    pub generation: Option<MessageGeneration>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MessageStatus {
    Complete,
    Streaming { call_id: CallId },
    ProcessingTools,
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub total_tokens: usize,
    #[serde(default)]
    pub input_tokens: usize,
    #[serde(default)]
    pub output_tokens: usize,
    #[serde(default)]
    pub reasoning_tokens: usize,
    #[serde(default)]
    pub cache_read_tokens: usize,
}

impl TokenUsage {
    pub fn used_context_tokens(&self) -> usize {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.reasoning_tokens)
            .saturating_add(self.cache_read_tokens)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageGeneration {
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    #[serde(default)]
    pub max_context: Option<usize>,
    #[serde(default)]
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: CallId,
    pub tool_id: ToolId,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallGlobalConfig {
    pub workspace_dir: PathBuf,
    #[serde(default)]
    pub sandbox: SandboxConfig,
}

impl ToolCallGlobalConfig {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            sandbox: SandboxConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxConfig {
    #[serde(default)]
    pub mode: SandboxMode,
    #[serde(default)]
    pub network_access: NetworkAccess,
    #[serde(default)]
    pub writable_roots: Vec<PathBuf>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            mode: SandboxMode::WorkspaceWrite,
            network_access: NetworkAccess::Restricted,
            writable_roots: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    #[default]
    WorkspaceWrite,
    DangerFullAccess,
}

impl SandboxMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkAccess {
    #[default]
    Restricted,
    Enabled,
}

impl NetworkAccess {
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPermissions {
    #[default]
    UseDefault,
    RequireEscalated,
    WithAdditionalPermissions,
}

impl SandboxPermissions {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "use_default" => Some(Self::UseDefault),
            "require_escalated" => Some(Self::RequireEscalated),
            "with_additional_permissions" => Some(Self::WithAdditionalPermissions),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::UseDefault => "use_default",
            Self::RequireEscalated => "require_escalated",
            Self::WithAdditionalPermissions => "with_additional_permissions",
        }
    }

    pub fn requires_escalated_permissions(self) -> bool {
        matches!(self, Self::RequireEscalated)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskLevel {
    ReadOnlyWorkspace = 0,
    UndoableWorkspaceWrite = 1,
    NonUndoableWorkspaceWrite = 2,
    ReadOnlyOutsideWorkspace = 3,
    WriteOutsideWorkspace = 4,
}

impl RiskLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnlyWorkspace => "read_only_workspace",
            Self::UndoableWorkspaceWrite => "undoable_workspace_write",
            Self::NonUndoableWorkspaceWrite => "non_undoable_workspace_write",
            Self::ReadOnlyOutsideWorkspace => "read_only_outside_workspace",
            Self::WriteOutsideWorkspace => "write_outside_workspace",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "read_only_workspace" => Some(Self::ReadOnlyWorkspace),
            "undoable_workspace_write" => Some(Self::UndoableWorkspaceWrite),
            "non_undoable_workspace_write" => Some(Self::NonUndoableWorkspaceWrite),
            "read_only_outside_workspace" => Some(Self::ReadOnlyOutsideWorkspace),
            "write_outside_workspace" => Some(Self::WriteOutsideWorkspace),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentProfileSource {
    BuiltIn,
    Global,
    Workspace,
}

impl AgentProfileSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BuiltIn => "built_in",
            Self::Global => "global",
            Self::Workspace => "workspace",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfileSummary {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub default_risk_level: RiskLevel,
    pub source: AgentProfileSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfileWarning {
    pub source: AgentProfileSource,
    pub path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfilesState {
    pub profiles: Vec<AgentProfileSummary>,
    pub warnings: Vec<AgentProfileWarning>,
    pub selected_profile_id: Option<String>,
    pub profile_locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionPolicy {
    AutonomousUpTo(RiskLevel),
    AlwaysAsk,
    NeverAllow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallAssessment {
    pub risk: RiskLevel,
    pub policy: ExecutionPolicy,
    pub reasons: Vec<String>,
}

impl ToolCallAssessment {
    pub fn is_auto_approved(&self, threshold: RiskLevel) -> bool {
        self.is_autonomous_policy_approved() && self.risk <= threshold
    }

    pub fn is_autonomous_policy_approved(&self) -> bool {
        matches!(
            self.policy,
            ExecutionPolicy::AutonomousUpTo(max_risk) if self.risk <= max_risk
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: CallId,
    pub tool_id: ToolId,
    pub output: serde_json::Value,
    pub permission_denied: bool,
    #[serde(default)]
    pub tool_state_deltas: Vec<ToolStateDelta>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStateSnapshot {
    #[serde(default)]
    pub entries: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStateDelta {
    pub namespace: String,
    pub operation: String,
    pub payload: serde_json::Value,
}

pub fn format_tool_result_message(
    tool_id: &ToolId,
    output: &serde_json::Value,
    permission_denied: bool,
) -> String {
    if permission_denied {
        format!("Tool '{tool_id}' was denied by user")
    } else {
        let output_str = serde_json::to_string_pretty(output).unwrap_or_else(|_| "{}".to_string());
        format!("Tool '{tool_id}' result:\n{output_str}")
    }
}

/// Wrapper that gives type safety while keeping Arc<str> benefits
macro_rules! define_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub Arc<str>);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self::try_new(s).expect(concat!(stringify!($name), " contains invalid characters"))
            }

            pub fn try_new(s: impl Into<String>) -> Result<Self, String> {
                let s = s.into();
                validate_id(&s)?;
                Ok(Self(Arc::from(s)))
            }

            pub fn as_str(&self) -> &str {
                self.0.as_ref()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let s = String::deserialize(deserializer)?;
                Self::try_new(s).map_err(serde::de::Error::custom)
            }
        }
    };
}

define_id!(MessageId);
define_id!(SessionId);
define_id!(CallId);
define_id!(ToolId);
define_id!(ProviderId);
define_id!(ModelId);
