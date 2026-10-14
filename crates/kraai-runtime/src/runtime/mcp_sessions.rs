use futures::{StreamExt, stream};

use super::core::RuntimeCore;

impl RuntimeCore {
    pub(crate) async fn shutdown_mcp(&self) {
        let managers = self.agent_manager.take_mcp_managers();
        stream::iter(managers)
            .for_each_concurrent(8, |manager| async move {
                manager.shutdown().await;
            })
            .await;
    }
}
