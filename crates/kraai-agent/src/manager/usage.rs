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

    async fn usage_order(
        &self,
        message_id: &MessageId,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let order = self
            .streaming_messages
            .read()
            .await
            .get(message_id)?
            .usage_order
            .clone();
        Some(order.lock_owned().await)
    }

    pub async fn record_request_started(
        &self,
        message_id: &MessageId,
    ) -> AgentResult<Option<kraai_types::RequestUsage>> {
        let missing = || eyre!("Missing streaming message");
        let _order = self.usage_order(message_id).await.ok_or_else(missing)?;
        let streaming = self.streaming_messages.read().await;
        let state = streaming.get(message_id).ok_or_else(missing)?;
        let session_id = state.session_id.clone();
        let request = Self::request_usage(state, None);
        drop(streaming);
        if let Some(request) = &request {
            self.usage_store.save(&session_id, request).await?;
        }
        Ok(request)
    }

    pub async fn record_request_attempt(
        &self,
        message_id: &MessageId,
        unpriced_attempts: u32,
    ) -> AgentResult<Option<kraai_types::RequestUsage>> {
        let missing = || eyre!("Missing streaming message");
        let _order = self.usage_order(message_id).await.ok_or_else(missing)?;
        let streaming = self.streaming_messages.read().await;
        let state = streaming.get(message_id).ok_or_else(missing)?;
        let session_id = state.session_id.clone();
        let unpriced_attempts = state.reissued_attempts.saturating_add(unpriced_attempts);
        let mut request = Self::request_usage(state, None);
        drop(streaming);
        if let Some(request) = &mut request {
            request.unpriced_attempts = unpriced_attempts;
            self.usage_store.save(&session_id, request).await?;
            if let Some(state) = self.streaming_messages.write().await.get_mut(message_id) {
                state.unpriced_attempts = unpriced_attempts;
            }
        }
        Ok(request)
    }

    pub async fn reset_streaming_message(
        &self,
        message_id: &MessageId,
    ) -> AgentResult<Option<Option<kraai_types::RequestUsage>>> {
        let Some(_order) = self.usage_order(message_id).await else {
            return Ok(None);
        };
        let mut streaming = self.streaming_messages.write().await;
        let Some(state) = streaming.get_mut(message_id) else {
            return Ok(None);
        };
        state.record(kraai_persistence::StreamDelta::Reset)?;
        state.text_item_ids.clear();
        state.reissued_attempts = state.unpriced_attempts.saturating_add(1);
        state.unpriced_attempts = state.reissued_attempts;
        let session_id = state.session_id.clone();
        let request = Self::request_usage(state, None);
        drop(streaming);
        if let Some(request) = &request {
            self.usage_store.save(&session_id, request).await?;
        }
        Ok(Some(request))
    }

    pub async fn set_streaming_message_usage(
        &self,
        message_id: &MessageId,
        usage: TokenUsage,
    ) -> AgentResult<Option<kraai_types::RequestUsage>> {
        let Some(_order) = self.usage_order(message_id).await else {
            return Ok(None);
        };
        let streaming = self.streaming_messages.read().await;
        let Some(state) = streaming.get(message_id) else {
            return Ok(None);
        };
        let session_id = state.session_id.clone();
        let request = Self::request_usage(state, Some(usage.clone()));
        drop(streaming);
        let Some(request) = request else {
            return Ok(None);
        };
        self.usage_store.save(&session_id, &request).await?;
        if let Some(state) = self.streaming_messages.write().await.get_mut(message_id) {
            state.record(kraai_persistence::StreamDelta::Usage { usage })?;
        }
        Ok(Some(request))
    }
}
