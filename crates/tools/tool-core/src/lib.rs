use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use types::ToolId;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("Tool not found: {0}")]
    ToolNotFound(ToolId),
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
        let data = serde_json::to_value(data).unwrap();
        Self::Success { data }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    fn schema(&self) -> &'static str;

    async fn call(&self, args: serde_json::Value) -> ToolOutput;
}

#[derive(Default, Clone)]
pub struct ToolManager {
    tools: BTreeMap<ToolId, Arc<dyn Tool>>,
}

impl ToolManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_tool(&mut self, tool: impl Tool + 'static) {
        let id = ToolId::new(tool.name());
        self.tools.insert(id, Arc::new(tool));
    }

    pub fn has_tool(&self, id: &ToolId) -> bool {
        self.tools.contains_key(id)
    }

    pub fn get_tool(&self, id: &ToolId) -> Option<Arc<dyn Tool>> {
        self.tools.get(id).cloned()
    }

    pub fn list_tools(&self) -> Vec<ToolId> {
        self.tools.keys().cloned().collect()
    }

    pub async fn call_tool(
        &self,
        id: &ToolId,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .tools
            .get(id)
            .ok_or_else(|| ToolError::ToolNotFound(id.clone()))?;
        Ok(tool.call(args).await)
    }
}
