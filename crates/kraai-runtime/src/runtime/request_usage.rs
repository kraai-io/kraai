use super::core::emit_event;
use crate::{api::Event, handle::RuntimeEventSender};
use kraai_agent::AgentManager;
use kraai_provider_core::{ProviderRetryEvent, ProviderRetryObserver};
use kraai_types::{MessageId, ModelId, ProviderId};
use std::{pin::Pin, sync::Arc};
use tokio::sync::RwLock;

pub(super) struct RuntimeRetryObserver {
    pub(super) session_id: String,
    pub(super) provider_id: ProviderId,
    pub(super) model_id: ModelId,
    pub(super) event_tx: RuntimeEventSender,
    pub(super) message_id: MessageId,
    pub(super) agent_manager: Arc<RwLock<AgentManager>>,
    pub(super) session_state_barrier: Arc<RwLock<()>>,
}

impl ProviderRetryObserver for RuntimeRetryObserver {
    fn on_retry_scheduled(&self, event: &ProviderRetryEvent) {
        emit_event(
            &self.event_tx,
            Event::ProviderRetryScheduled {
                session_id: self.session_id.clone(),
                provider_id: self.provider_id.to_string(),
                model_id: self.model_id.to_string(),
                operation: event.operation.to_string(),
                retry_number: event.retry_number,
                delay_seconds: event.delay.as_secs(),
                reason: event.reason.clone(),
            },
        );
    }
    fn before_attempt(
        &self,
        unpriced_prior_attempts: u32,
    ) -> Pin<Box<dyn Future<Output = color_eyre::Result<()>> + Send + '_>> {
        Box::pin(async move {
            if unpriced_prior_attempts == 0 {
                return Ok(());
            }
            let _guard = self.session_state_barrier.read().await;
            let agent = self.agent_manager.read().await;
            if let Some(request) = agent
                .record_request_attempt(&self.message_id, unpriced_prior_attempts)
                .await?
            {
                emit_event(
                    &self.event_tx,
                    Event::RequestUsageUpdated {
                        session_id: self.session_id.clone(),
                        request: Box::new(request),
                    },
                );
            }
            drop(agent);
            Ok(())
        })
    }
}
