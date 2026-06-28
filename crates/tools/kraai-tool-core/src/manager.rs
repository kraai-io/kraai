use std::{collections::BTreeMap, sync::Arc};

use kraai_types::ToolId;

use crate::prepared::{ErasedTool, TypedToolAdapter};
use crate::{PreparedToolCall, ToolError, TypedTool};

#[derive(Default, Clone)]
pub struct ToolManager {
    tools: BTreeMap<ToolId, Arc<dyn ErasedTool>>,
}

impl ToolManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_tool<T>(&mut self, tool: T)
    where
        T: TypedTool,
    {
        let id = ToolId::new(tool.name());
        self.tools.insert(id, Arc::new(TypedToolAdapter::new(tool)));
    }

    pub fn has_tool(&self, id: &ToolId) -> bool {
        self.tools.contains_key(id)
    }

    pub fn list_tools(&self) -> Vec<ToolId> {
        self.tools.keys().cloned().collect()
    }

    pub fn generate_system_prompt(&self) -> String {
        self.tools
            .values()
            .map(|t| t.schema())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn generate_system_prompt_for_tools(
        &self,
        tool_ids: &[ToolId],
    ) -> Result<String, ToolError> {
        let mut sections = Vec::with_capacity(tool_ids.len());
        for tool_id in tool_ids {
            let tool = self
                .tools
                .get(tool_id)
                .ok_or_else(|| ToolError::ToolNotFound(tool_id.clone()))?;
            sections.push(tool.schema());
        }
        Ok(sections.join("\n\n"))
    }

    pub fn prepare_tool(
        &self,
        id: &ToolId,
        args: serde_json::Value,
    ) -> Result<PreparedToolCall, ToolError> {
        let tool = self
            .tools
            .get(id)
            .ok_or_else(|| ToolError::ToolNotFound(id.clone()))?;
        tool.prepare(id, args)
    }
}
