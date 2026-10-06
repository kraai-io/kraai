use rmcp::model::{CancelledNotificationParam, RequestId};
use rmcp::{Peer, RoleClient};

pub(crate) struct CancelOnDrop {
    request: Option<(Peer<RoleClient>, RequestId)>,
}

impl CancelOnDrop {
    pub(crate) fn new(peer: Peer<RoleClient>, id: RequestId) -> Self {
        Self {
            request: Some((peer, id)),
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.request = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some((peer, request_id)) = self.request.take() {
            tokio::spawn(async move {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    peer.notify_cancelled(CancelledNotificationParam::new(
                        Some(request_id),
                        Some(String::from("Kraai script stopped")),
                    )),
                )
                .await;
            });
        }
    }
}
