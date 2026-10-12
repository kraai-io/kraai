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
        let unpriced_attempts = state.reissued_attempts.saturating_add(unpriced_attempts);
        let mut request = Self::request_usage(state, None);
        if let Some(request) = &mut request {
            request.unpriced_attempts = unpriced_attempts;
            self.usage_store.save(&state.session_id, request).await?;
            state.unpriced_attempts = unpriced_attempts;
        }
        drop(streaming);
        Ok(request)
    }

    /// Discards a stream's partial response before its request is issued again, counting the
    /// failed attempt as unpriced. Returns `None` when the stream is no longer active.
    pub async fn reset_streaming_message(
        &self,
        message_id: &MessageId,
    ) -> Result<Option<Option<kraai_types::RequestUsage>>> {
        let mut streaming = self.streaming_messages.write().await;
        let Some(state) = streaming.get_mut(message_id) else {
            return Ok(None);
        };
        state.record(kraai_persistence::StreamDelta::Reset)?;
        state.text_item_ids.clear();
        state.reissued_attempts = state.unpriced_attempts.saturating_add(1);
        state.unpriced_attempts = state.reissued_attempts;
        let request = Self::request_usage(state, None);
        if let Some(request) = &request {
            self.usage_store.save(&state.session_id, request).await?;
        }
        drop(streaming);
        Ok(Some(request))
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
            if state.message.generation.is_some() {
                state.record(kraai_persistence::StreamDelta::Usage { usage })?;
            }
            drop(streaming);
            return Ok(Some(request));
        }
        drop(streaming);
        Ok(None)
    }
}
