use super::*;

impl AgentManager {
    pub async fn prepare_messages_stream(
        &mut self,
        session_id: &SessionId,
        messages: Vec<kraai_types::MessageContent>,
        model_id: ModelId,
        provider_id: ProviderId,
        options: ModelOptionValues,
    ) -> Result<Option<PendingStreamRequest>> {
        self.validate_model_options(&provider_id, &model_id, &options, true)
            .await?;
        let newly_claimed = !self.persistence.sessions().owns_turn(session_id).await?;
        if newly_claimed {
            self.persistence.sessions().claim_turn(session_id).await?;
        }
        let result = self
            .prepare_claimed_messages_stream(session_id, messages, model_id, provider_id, options)
            .await;
        if newly_claimed
            && !matches!(result, Ok(Some(_)))
            && !self.is_turn_active(session_id)
            && !self.session_has_active_stream(session_id).await
            && let Err(error) = self.persistence.sessions().release_turn(session_id).await
        {
            tracing::warn!(%session_id, %error, "Failed to release session after stream preparation");
        }
        result
    }

    async fn prepare_claimed_messages_stream(
        &mut self,
        session_id: &SessionId,
        messages: Vec<kraai_types::MessageContent>,
        model_id: ModelId,
        provider_id: ProviderId,
        options: ModelOptionValues,
    ) -> Result<Option<PendingStreamRequest>> {
        if self.session_has_active_stream(session_id).await {
            return Ok(None);
        }
        let session = self
            .recover_interrupted_stream(self.require_session(session_id).await?)
            .await?;
        if messages.is_empty() && session.tip_id.is_none() {
            return Ok(None);
        }
        let selected_profile = Arc::new(self.resolve_selected_profile(&session)?);
        self.persist_model_selection(
            session_id,
            &kraai_types::ModelSelection {
                provider_id: provider_id.clone(),
                model_id: model_id.clone(),
                options: options.clone(),
            },
        )
        .await?;
        let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
        let previous_state = state.clone();
        if state.active_turn_profile.is_none() {
            state.promote_pending_workspace_dir();
            state.active_turn_profile = Some(selected_profile.clone());
        }
        state.last_model = Some(model_id);
        state.last_provider = Some(provider_id);
        state.last_options = options;
        let profile = state
            .active_turn_profile
            .clone()
            .unwrap_or(selected_profile);

        let result = self.prepare_turn_stream(session_id, messages).await;
        if matches!(result, Ok(Some(_))) {
            self.last_used_profile_id = Some(profile.id.clone());
        } else {
            self.session_states
                .insert(session_id.clone(), previous_state);
        }
        result
    }
}
