use super::*;

impl AgentManager {
    /// Append queued messages after existing script results and prepare one provider call.
    /// Failed preparation restores turn settings and rolls back the appended history.
    pub async fn prepare_intercepted_stream(
        &mut self,
        session_id: &str,
        messages: Vec<String>,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> Result<Option<PendingStreamRequest>> {
        self.finish_pending_message_rollback(session_id).await?;
        if messages.is_empty() {
            return self.prepare_continuation_stream(session_id).await;
        }
        if self.session_has_active_stream(session_id).await {
            return Ok(None);
        }
        let session = self
            .recover_interrupted_stream(self.require_session(session_id).await?)
            .await?;
        let selected_profile = self.resolve_selected_profile(&session)?;
        let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
        let previous_state = state.clone();
        if state.active_turn_profile.is_none() {
            state.promote_pending_workspace_dir();
            state.active_turn_profile = Some(selected_profile.clone());
        }
        state.last_model = Some(model_id);
        state.last_provider = Some(provider_id);
        let profile = state
            .active_turn_profile
            .clone()
            .unwrap_or(selected_profile);

        let mut appended = Vec::with_capacity(messages.len());
        let result = async {
            for message in messages {
                appended.push(
                    self.append_message(
                        session_id,
                        ChatRole::User,
                        message,
                        Some(profile.id.clone()),
                    )
                    .await?,
                );
            }
            self.prepare_continuation_stream(session_id).await
        }
        .await;

        if !matches!(result, Ok(Some(_))) {
            self.session_states
                .insert(session_id.to_string(), previous_state);
            self.pending_message_rollbacks
                .insert(session_id.to_string(), appended);
            if let Err(rollback_error) = self.finish_pending_message_rollback(session_id).await {
                return Err(match result {
                    Err(error) => {
                        rollback_error.wrap_err(format!("Stream preparation failed: {error}"))
                    }
                    _ => rollback_error,
                });
            }
        }
        result
    }

    /// Resume an unfinished rollback before any stream preparation can append history.
    /// Keep each entry until its persisted tip has been restored successfully.
    pub(super) async fn finish_pending_message_rollback(&mut self, session_id: &str) -> Result<()> {
        if let Some(messages) = self.pending_message_rollbacks.get_mut(session_id) {
            while let Some(message) = messages.last() {
                self.conversation_store
                    .restore_appended_message(session_id, message)
                    .await
                    .map_err(|error| {
                        error.wrap_err(
                            "Queued message rollback is incomplete; stream preparation is blocked",
                        )
                    })?;
                messages.pop();
            }
        }
        self.pending_message_rollbacks.remove(session_id);
        Ok(())
    }
}
