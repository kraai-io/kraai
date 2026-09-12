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
            unpriced_attempts: state.unpriced_attempts,
            usage,
        })
    }

    pub async fn record_request_started(
        &self,
        message_id: &MessageId,
    ) -> Result<Option<kraai_types::RequestUsage>> {
        let streaming = self.streaming_messages.read().await;
        let state = streaming
            .get(message_id)
            .ok_or_else(|| eyre!("Missing streaming message"))?;
        let request = Self::request_usage(state, None);
        if let Some(request) = &request {
            self.usage_store.save(&state.session_id, request).await?;
        }
        drop(streaming);
        Ok(request)
    }

    pub async fn record_request_attempt(
        &self,
        message_id: &MessageId,
        unpriced_attempts: u32,
    ) -> Result<Option<kraai_types::RequestUsage>> {
        let mut streaming = self.streaming_messages.write().await;
        let state = streaming
            .get_mut(message_id)
            .ok_or_else(|| eyre!("Missing streaming message"))?;
        let mut request = Self::request_usage(state, None);
        if let Some(request) = &mut request {
            request.unpriced_attempts = unpriced_attempts;
            self.usage_store.save(&state.session_id, request).await?;
            state.unpriced_attempts = unpriced_attempts;
        }
        drop(streaming);
        Ok(request)
    }

    pub async fn set_streaming_message_usage(
        &self,
        message_id: &MessageId,
        usage: TokenUsage,
    ) -> Result<Option<kraai_types::RequestUsage>> {
        let mut streaming = self.streaming_messages.write().await;
        if let Some(state) = streaming.get_mut(message_id)
            && let Some(request) = Self::request_usage(state, Some(usage.clone()))
        {
            self.usage_store.save(&state.session_id, &request).await?;
            if let Some(generation) = state.message.generation.as_mut() {
                generation.usage = Some(usage);
            }
            drop(streaming);
            return Ok(Some(request));
        }
        drop(streaming);
        Ok(None)
    }
}
