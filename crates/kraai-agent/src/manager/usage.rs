use super::*;

impl AgentManager {
    fn request_usage(
        state: &StreamingMessageState,
        usage: Option<TokenUsage>,
    ) -> Option<kraai_types::RequestUsage> {
        let generation = state.message.generation.as_ref()?;
        Some(kraai_types::RequestUsage {
            message_id: state.message.id.clone(),
            provider_id: generation.provider_id.clone(),
            model_id: generation.model_id.clone(),
            started_at: state.request_started_at,
            subscription: state.subscription,
            usage,
        })
    }

    pub async fn record_request_started(&self, message_id: &MessageId) -> Result<()> {
        let streaming = self.streaming_messages.read().await;
        let state = streaming
            .get(message_id)
            .ok_or_else(|| eyre!("Missing streaming message"))?;
        if let Some(request) = Self::request_usage(state, None) {
            self.usage_store.save(&state.session_id, &request).await?;
        }
        drop(streaming);
        Ok(())
    }

    pub async fn set_streaming_message_usage(
        &self,
        message_id: &MessageId,
        usage: TokenUsage,
    ) -> Result<bool> {
        let mut streaming = self.streaming_messages.write().await;
        if let Some(state) = streaming.get_mut(message_id)
            && let Some(request) = Self::request_usage(state, Some(usage.clone()))
        {
            self.usage_store.save(&state.session_id, &request).await?;
            if let Some(generation) = state.message.generation.as_mut() {
                generation.usage = Some(usage);
            }
            drop(streaming);
            return Ok(true);
        }
        drop(streaming);
        Ok(false)
    }
}
