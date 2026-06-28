use async_trait::async_trait;
use kraai_types::{
    ExecutionPolicy, RiskLevel, ToolCallAssessment, ToolCallGlobalConfig, ToolId, ToolStateDelta,
    ToolStateSnapshot,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("Tool not found: {0}")]
    ToolNotFound(ToolId),
    #[error("{0}")]
    Preparation(String),
    #[error("Failed to serialize tool output: {0}")]
    OutputSerialization(#[from] serde_json::Error),
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum ToolOutput {
    Success {
        #[serde(flatten)]
        data: serde_json::Value,
    },
    Error {
        message: String,
    },
}

impl ToolOutput {
    pub fn error(message: String) -> Self {
        Self::Error { message }
    }

    pub fn success<D: Serialize>(data: D) -> Self {
        match serde_json::to_value(data) {
            Ok(data) => Self::Success { data },
            Err(error) => Self::error(format!("failed to serialize tool output: {error}")),
        }
    }
}

pub struct ToolCallResult {
    pub output: ToolOutput,
    pub tool_state_deltas: Vec<ToolStateDelta>,
}

impl ToolCallResult {
    pub fn error(message: String) -> Self {
        Self {
            output: ToolOutput::error(message),
            tool_state_deltas: Vec::new(),
        }
    }

    pub fn success<D: Serialize>(data: D) -> Self {
        Self::success_with_deltas(data, Vec::new())
    }

    pub fn success_with_deltas<D: Serialize>(
        data: D,
        tool_state_deltas: Vec<ToolStateDelta>,
    ) -> Self {
        let output = ToolOutput::success(data);
        let tool_state_deltas = match output {
            ToolOutput::Success { .. } => tool_state_deltas,
            ToolOutput::Error { .. } => Vec::new(),
        };
        Self {
            output,
            tool_state_deltas,
        }
    }
}

pub struct ToolContext<'a> {
    pub global_config: &'a ToolCallGlobalConfig,
    pub tool_state_snapshot: &'a ToolStateSnapshot,
}

#[async_trait]
pub trait TypedTool: Send + Sync + Clone + 'static {
    type Args: DeserializeOwned + Send + Sync + Clone + 'static;

    fn name(&self) -> &'static str;

    fn schema(&self) -> &'static str;

    fn assess(&self, _args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
        ToolCallAssessment {
            risk: RiskLevel::WriteOutsideWorkspace,
            policy: ExecutionPolicy::AlwaysAsk,
            reasons: vec![String::from(
                "Tool does not define a custom autonomy policy",
            )],
        }
    }

    fn describe(&self, _args: &Self::Args) -> String {
        format!("{}: <typed args>", self.name())
    }

    async fn call(&self, args: Self::Args, ctx: &ToolContext<'_>) -> ToolCallResult;
}
