use super::*;

impl AgentManager {
    pub fn session_mcp(&self, session_id: &str) -> Arc<kraai_mcp::McpManager> {
        self.session_mcp
            .get(session_id)
            .unwrap_or(&self.mcp)
            .clone()
    }

    pub async fn replace_session_mcp_servers(
        &mut self,
        session_id: &str,
        config: kraai_mcp::McpConfig,
    ) -> Result<Option<Arc<kraai_mcp::McpManager>>> {
        self.require_session(session_id).await?;
        self.persistence
            .sessions()
            .ensure_writable(session_id)
            .await?;
        if self.is_turn_active(session_id) {
            return Err(eyre!(kraai_types::DomainError::conflict(
                "Cannot change MCP servers while the current turn is active"
            )));
        }
        let empty = config.servers.is_empty();
        let mcp = self
            .mcp
            .with_session_servers(config)
            .map_err(|error| eyre!(kraai_types::DomainError::invalid_argument(error)))?;
        Ok(if empty {
            self.session_mcp.remove(session_id)
        } else {
            self.session_mcp
                .insert(session_id.to_owned(), Arc::new(mcp))
        })
    }

    pub fn take_mcp_managers(&mut self) -> Vec<Arc<kraai_mcp::McpManager>> {
        std::mem::take(&mut self.session_mcp)
            .into_values()
            .chain(std::iter::once(std::mem::take(&mut self.mcp)))
            .collect()
    }
}
