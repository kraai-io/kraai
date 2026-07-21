#![forbid(unsafe_code)]

mod permissions;
mod policy;
mod profile;
mod script;

pub use permissions::{SandboxCapabilities, SandboxCapability, SandboxCapabilityError};
pub use policy::{
    CapabilityPermissionRules, EscalationPolicy, PermissionResolution, ResolvedPermissions,
    SandboxPermissionSet,
};
pub use profile::{EnvironmentPolicy, NushellStartup, PathPolicy, ScriptProfileSnapshot};
pub use script::{ScriptExecutionPhase, ScriptExecutionStatus, ScriptOutputStream};

use serde::{Deserialize, Serialize};
use std::sync::Arc;

fn validate_id(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("id cannot be empty".to_string());
    }

    Ok(())
}

fn validate_message_id(value: &str) -> Result<(), String> {
    validate_id(value)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "message id may contain only ASCII letters, digits, hyphens, and underscores"
                .to_string(),
        );
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
    #[serde(rename = "tool_call_result")]
    ToolCallResult,
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
    pub generation: Option<MessageGeneration>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MessageStatus {
    Complete,
    Streaming { stream_id: StreamId },
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
    pub commands: Vec<String>,
    pub capabilities: SandboxCapabilities,
    pub escalation_policy: EscalationPolicy,
    pub environment: EnvironmentPolicy,
    pub nushell_startup: NushellStartup,
    pub path: PathPolicy,
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
pub struct ContextStateDelta {
    pub namespace: String,
    pub operation: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateEffectRequest {
    pub sequence: u64,
    pub invocation_id: CommandInvocationId,
    pub command_id: String,
    pub deltas: Vec<ContextStateDelta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateEffectAck {
    pub invocation_id: CommandInvocationId,
    pub error: Option<String>,
}

/// Wrapper that gives type safety while keeping Arc<str> benefits
macro_rules! define_id {
    ($name:ident, $validator:path) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub Arc<str>);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self::try_new(s).expect(concat!(stringify!($name), " contains invalid characters"))
            }

            pub fn try_new(s: impl Into<String>) -> Result<Self, String> {
                let s = s.into();
                $validator(&s)?;
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

define_id!(MessageId, validate_message_id);
define_id!(SessionId, validate_id);
define_id!(StreamId, validate_id);
define_id!(ScriptExecutionId, validate_message_id);
define_id!(CommandInvocationId, validate_message_id);
define_id!(ProviderId, validate_id);
define_id!(ModelId, validate_id);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_ids_reject_path_syntax() {
        for invalid in [
            "../sessions",
            "/tmp/message",
            r"..\sessions",
            r"C:\temp\message",
            r"\\server\share",
            ".",
        ] {
            assert!(MessageId::try_new(invalid).is_err(), "accepted {invalid:?}");
            assert!(
                ScriptExecutionId::try_new(invalid).is_err(),
                "accepted {invalid:?}"
            );
            assert!(
                CommandInvocationId::try_new(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
        assert!(MessageId::try_new("01J_VALID-message_id").is_ok());
        assert!(ScriptExecutionId::try_new("01J_VALID-execution_id").is_ok());
        assert!(CommandInvocationId::try_new("01J_VALID-invocation_id").is_ok());
    }
}
