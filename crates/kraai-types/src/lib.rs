mod context_state;
mod cost;
mod model;
mod model_options;
pub use model::ModelSelection;
pub use model_options::{
    ModelOptionBinding, ModelOptionChoice, ModelOptionCondition, ModelOptionDefinition,
    ModelOptionError, ModelOptionKind, ModelOptionValue, ModelOptionValues, ModelRequestPatch,
    REASONING_EFFORT_OPTION_ID, parse_model_option_assignments, reconcile_model_option_values,
    validate_model_option_values, validate_model_options,
};

pub use context_state::{
    ContextStateEvent, ContextStateEventSource, ContextStateMutation, PinnedFileScope,
};
pub use cost::{CostSummary, RequestCost, RequestUsage, TokenRates, Usd};

use kraai_execution_types::{
    EnvironmentPolicy, EscalationPolicy, NushellStartup, PathPolicy, SandboxCapabilities,
    define_id, validate_id, validate_message_id,
};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

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

pub use conversation::{
    AssistantItem, AssistantPhase, ContentPart, ConversationItem, MessageContent, ScriptCall,
    ScriptRewrite,
};
mod conversation;
mod token_estimate;
pub use token_estimate::{BYTES_PER_TOKEN, estimate_text_tokens};

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub parent_id: Option<MessageId>,
    pub content: ConversationItem,
    pub status: MessageStatus,
    #[serde(default)]
    pub agent_profile_id: Option<String>,
    #[serde(default)]
    pub generation: Option<MessageGeneration>,
}

impl Message {
    pub fn role(&self) -> ChatRole {
        self.content.role()
    }

    pub fn display_text(&self) -> Cow<'_, str> {
        self.content.display_text()
    }
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MessageStatus {
    Complete,
    Streaming { stream_id: StreamId },
    Cancelled,
    Failed,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
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
    #[serde(default)]
    pub cache_write_tokens: usize,
    #[serde(default)]
    pub cost: Option<RequestCost>,
}

impl TokenUsage {
    pub fn used_context_tokens(&self) -> usize {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.reasoning_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageGeneration {
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    #[serde(default)]
    pub options: ModelOptionValues,
    #[serde(default)]
    pub max_context: Option<usize>,
    #[serde(default)]
    pub usage: Option<TokenUsage>,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
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

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
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

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfileWarning {
    pub source: AgentProfileSource,
    pub path: Option<String>,
    pub message: String,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfilesState {
    pub profiles: Vec<AgentProfileSummary>,
    pub warnings: Vec<AgentProfileWarning>,
    pub selected_profile_id: Option<String>,
    pub profile_locked: bool,
}

define_id!(MessageId, validate_message_id);
define_id!(SessionId, validate_message_id);
define_id!(StreamId, validate_id);
define_id!(ToolCallId, validate_id);
define_id!(ProviderId, validate_id);
define_id!(ModelId, validate_id);

#[cfg(test)]
mod tests;
