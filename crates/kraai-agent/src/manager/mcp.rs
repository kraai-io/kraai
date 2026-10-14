use super::*;

impl AgentManager {
    pub fn session_mcp(&self, session_id: &SessionId) -> Arc<kraai_mcp::McpManager> {
        lock(&self.session_mcp)
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| self.mcp())
    }

    pub async fn replace_session_mcp_servers(
        &self,
        session_id: &SessionId,
        config: kraai_mcp::McpConfig,
    ) -> AgentResult<Option<Arc<kraai_mcp::McpManager>>> {
        self.require_session(session_id).await?;
        self.persistence
            .sessions()
            .ensure_writable(session_id)
            .await?;
        if self.is_turn_active(session_id) {
            return Err(AgentError::conflict(
                "Cannot change MCP servers while the current turn is active",
            ));
        }
        let empty = config.servers.is_empty();
        let mcp = self
            .mcp()
            .with_session_servers(config)
            .map_err(AgentError::invalid_argument)?;
        Ok(if empty {
            lock(&self.session_mcp).remove(session_id)
        } else {
            lock(&self.session_mcp).insert(session_id.clone(), Arc::new(mcp))
        })
    }

    pub fn take_mcp_managers(&self) -> Vec<Arc<kraai_mcp::McpManager>> {
        let sessions = std::mem::take(&mut *lock(&self.session_mcp));
        sessions
            .into_values()
            .chain(std::iter::once(std::mem::take(&mut *write(&self.mcp))))
            .collect()
    }
}
