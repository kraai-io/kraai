use super::*;

impl AgentManager {
    pub async fn prepare_messages_stream(
        &self,
        session_id: &SessionId,
        messages: Vec<kraai_types::MessageContent>,
        model_id: ModelId,
        provider_id: ProviderId,
        options: ModelOptionValues,
    ) -> AgentResult<Option<PendingStreamRequest>> {
        let selection = kraai_types::ModelSelection {
            provider_id,
            model_id,
            options,
        };
        self.prepare_users_stream(session_id, messages, selection, None)
            .await
    }

    pub async fn prepare_queued_stream(
        &self,
        session_id: &SessionId,
        queued: Vec<kraai_persistence::QueuedEntry>,
        selection: kraai_types::ModelSelection,
    ) -> AgentResult<Option<PendingStreamRequest>> {
        let consumed_queue_through = queued.last().map(|entry| entry.sequence);
        let messages = queued
            .into_iter()
            .map(|entry| entry.message.content)
            .collect();
        self.prepare_users_stream(session_id, messages, selection, consumed_queue_through)
            .await
    }

    async fn prepare_users_stream(
        &self,
        session_id: &SessionId,
        messages: Vec<kraai_types::MessageContent>,
        selection: kraai_types::ModelSelection,
        consumed_queue_through: Option<i64>,
    ) -> AgentResult<Option<PendingStreamRequest>> {
        self.validate_model_options(
            &selection.provider_id,
            &selection.model_id,
            &selection.options,
            true,
        )
        .await?;
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
        self.persist_model_selection(session_id, &selection).await?;
        let (previous_state, profile) =
            self.with_runtime_state(session_id, &session.workspace_dir, |state| {
                let previous_state = state.clone();
                if state.active_turn_profile.is_none() {
                    state.promote_pending_workspace_dir();
                    state.active_turn_profile = Some(selected_profile.clone());
                }
                state.last_model = Some(selection.model_id);
                state.last_provider = Some(selection.provider_id);
                state.last_options = selection.options;
                let profile = state
                    .active_turn_profile
                    .clone()
                    .unwrap_or(selected_profile);
                (previous_state, profile)
            });

        let result = self
            .prepare_turn_stream(session_id, messages, consumed_queue_through)
            .await;
        if matches!(result, Ok(Some(_))) {
            *lock(&self.last_used_profile_id) = Some(profile.id.clone());
        } else {
            lock(&self.session_states).insert(session_id.clone(), previous_state);
        }
        result
    }
}
