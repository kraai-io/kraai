use color_eyre::eyre::{Result, eyre};
use futures::{StreamExt, stream};

use super::core::RuntimeCore;
use kraai_types::{DomainError, SessionId};

impl RuntimeCore {
    pub(crate) async fn set_session_mcp_servers(
        &self,
        session_id: &SessionId,
        config: kraai_mcp::McpConfig,
    ) -> Result<()> {
        let Some(_preparation) = self.session_preparations.try_begin(session_id) else {
            return Err(eyre!(DomainError::conflict(
                "Cannot change MCP servers during prompt preparation"
            )));
        };
        if self.has_active_script_tasks(session_id).await
            || self
                .pending_script_approvals
                .lock()
                .await
                .contains_key(session_id)
            || self.active_streams.lock().await.contains_key(session_id)
            || self.session_store.has_queued_messages(session_id).await?
        {
            return Err(eyre!(DomainError::conflict(
                "Cannot change MCP servers while the session has pending work"
            )));
        }
        let previous = self
            .agent_manager
            .replace_session_mcp_servers(session_id, config)
            .await?;
        if let Some(previous) = previous {
            previous.shutdown().await;
        }
        Ok(())
    }

    pub(crate) async fn shutdown_mcp(&self) {
        let managers = self.agent_manager.take_mcp_managers();
        stream::iter(managers)
            .for_each_concurrent(8, |manager| async move {
                manager.shutdown().await;
            })
            .await;
    }
}
