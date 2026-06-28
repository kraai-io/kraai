use std::sync::Arc;

use async_trait::async_trait;
use kraai_types::{ToolCallAssessment, ToolId};

use crate::{ToolCallResult, ToolContext, ToolError, TypedTool};

#[async_trait]
trait PreparedToolCallInner: Send + Sync {
    fn assess(&self, ctx: &ToolContext<'_>) -> ToolCallAssessment;

    fn describe(&self) -> String;

    async fn call(&self, ctx: &ToolContext<'_>) -> ToolCallResult;
}

struct TypedPreparedToolCall<T: TypedTool> {
    tool: T,
    args: T::Args,
}

#[async_trait]
impl<T: TypedTool> PreparedToolCallInner for TypedPreparedToolCall<T> {
    fn assess(&self, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        self.tool.assess(&self.args, ctx)
    }

    fn describe(&self) -> String {
        self.tool.describe(&self.args)
    }

    async fn call(&self, ctx: &ToolContext<'_>) -> ToolCallResult {
        self.tool.call(self.args.clone(), ctx).await
    }
}

#[derive(Clone)]
pub struct PreparedToolCall {
    tool_id: ToolId,
    args_json: serde_json::Value,
    inner: Arc<dyn PreparedToolCallInner>,
}

impl PreparedToolCall {
    pub fn tool_id(&self) -> &ToolId {
        &self.tool_id
    }

    pub fn args_json(&self) -> &serde_json::Value {
        &self.args_json
    }

    pub fn assess(&self, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        self.inner.assess(ctx)
    }

    pub fn describe(&self) -> String {
        self.inner.describe()
    }

    pub async fn call(&self, ctx: &ToolContext<'_>) -> ToolCallResult {
        self.inner.call(ctx).await
    }
}

pub(crate) trait ErasedTool: Send + Sync {
    fn schema(&self) -> &'static str;

    fn prepare(
        &self,
        tool_id: &ToolId,
        args: serde_json::Value,
    ) -> Result<PreparedToolCall, ToolError>;
}

pub(crate) struct TypedToolAdapter<T: TypedTool> {
    tool: T,
}

impl<T: TypedTool> TypedToolAdapter<T> {
    pub(crate) fn new(tool: T) -> Self {
        Self { tool }
    }
}

impl<T: TypedTool> ErasedTool for TypedToolAdapter<T> {
    fn schema(&self) -> &'static str {
        self.tool.schema()
    }

    fn prepare(
        &self,
        tool_id: &ToolId,
        args: serde_json::Value,
    ) -> Result<PreparedToolCall, ToolError> {
        let parsed = serde_json::from_value::<T::Args>(args.clone()).map_err(|error| {
            ToolError::Preparation(format!(
                "Unable to validate {} arguments: {error}",
                self.tool.name()
            ))
        })?;

        Ok(PreparedToolCall {
            tool_id: tool_id.clone(),
            args_json: args,
            inner: Arc::new(TypedPreparedToolCall {
                tool: self.tool.clone(),
                args: parsed,
            }),
        })
    }
}
