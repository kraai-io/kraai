use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::core::RuntimeCore;
use crate::Event;

impl RuntimeCore {
    pub(crate) async fn spawn_mcp_auth_forwarder(&self) -> JoinHandle<()> {
        let manager = self.agent_manager.read().await.mcp();
        let mut updates = manager.subscribe_auth();
        let events = self.event_tx.clone();
        tokio::spawn(async move {
            loop {
                match updates.recv().await {
                    Ok(status) => events.send(Event::McpAuthUpdated { status }),
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        for status in manager.auth_statuses().await {
                            events.send(Event::McpAuthUpdated { status });
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    }
}
