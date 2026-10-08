use super::*;

impl AgentManager {
    pub async fn prepare_start_stream(
        &mut self,
        session_id: &str,
        message: kraai_types::MessageContent,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> Result<PendingStreamRequest> {
        if self.is_turn_active(session_id) {
            return Err(eyre!(kraai_types::DomainError::conflict(
                "Cannot send a new message while the current turn is active"
            )));
        }
        self.prepare_messages_stream(session_id, vec![message], model_id, provider_id)
            .await?
            .ok_or_else(|| eyre!("Failed to prepare a new message stream"))
    }

    pub async fn prepare_continuation_stream(
        &mut self,
        session_id: &str,
    ) -> Result<Option<PendingStreamRequest>> {
        self.finish_pending_message_rollback(session_id).await?;
        let session = self
            .recover_interrupted_stream(self.require_session(session_id).await?)
            .await?;
        let selected_profile = self.resolve_selected_profile(&session)?;
        let (model_id, provider_id, profile, workspace_dir) = {
            let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
            let Some(model_id) = &state.last_model else {
                return Ok(None);
            };
            let Some(provider_id) = &state.last_provider else {
                return Ok(None);
            };
            let profile = match state.active_turn_profile.clone() {
                Some(profile) => profile,
                None => {
                    let selected_profile = Arc::new(selected_profile);
                    state.active_turn_profile = Some(selected_profile.clone());
                    selected_profile
                }
            };
            (
                model_id.clone(),
                provider_id.clone(),
                profile,
                state.active_workspace_dir.clone(),
            )
        };

        if self.session_has_active_stream(session_id).await {
            return Ok(None);
        }

        let Some(tip_id) = self.get_tip(session_id).await? else {
            return Ok(None);
        };

        let context = self
            .get_model_history(&tip_id, (&provider_id, &model_id))
            .await?;
        self.providers.get_provider(&provider_id).ok_or_else(|| {
            kraai_provider_core::ProviderError::ProviderNotFound(provider_id.clone())
        })?;
        let mut system_prompt = self
            .build_turn_system_prompt(session_id, &profile, &workspace_dir)
            .await?;

        let max_context = self
            .resolve_model_max_context(&provider_id, &model_id)
            .await;
        let (provider_request, context_compaction, notifications) = self
            .build_model_context(
                session_id,
                context,
                &system_prompt,
                Some(ScriptToolDefinition::nushell()),
                max_context,
                (&provider_id, &model_id),
            )
            .await?;

        system_prompt.context_notifications.extend(notifications);
        let stream_id = StreamId::new(Ulid::generate());
        let generation = Some(MessageGeneration {
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            max_context,
            usage: None,
        });
        let assistant_msg_id = self
            .start_streaming_message(
                session_id,
                ChatRole::Assistant,
                stream_id,
                Some(profile.id.clone()),
                generation,
            )
            .await?;

        Ok(Some(PendingStreamRequest {
            message_id: assistant_msg_id,
            provider_id,
            model_id,
            provider_request,
            context_compaction,
            context_notifications: system_prompt.context_notifications,
        }))
    }
}
