use super::*;

const SCRIPT_TOOL_NAME: &str = "kraai_nushell";
const SCRIPT_TOOL_DESCRIPTION: &str = "Execute one complete Nushell script in Kraai's local, policy-controlled scripting environment. Input must be plaintext Nushell beginning with a metadata comment containing timeout.";

impl AgentManager {
    pub async fn prepare_start_stream(
        &mut self,
        session_id: &str,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> Result<PendingStreamRequest> {
        self.finish_pending_message_rollback(session_id).await?;
        let script_tool_transport = self
            .providers
            .script_tool_transport(&provider_id, &model_id)
            .unwrap_or(ScriptToolTransport::TextEnvelope);
        let session = self
            .recover_interrupted_stream(self.require_session(session_id).await?)
            .await?;
        let profile = Arc::new(self.resolve_selected_profile(&session)?);
        let workspace_dir = {
            let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
            if state.active_turn_profile.is_some() {
                return Err(eyre!(kraai_types::DomainError::conflict(
                    "Cannot send a new message while the current turn is active"
                )));
            }
            state.promote_pending_workspace_dir();
            state.last_model = Some(model_id.clone());
            state.last_provider = Some(provider_id.clone());
            state.active_turn_profile = Some(profile.clone());
            state.active_workspace_dir.clone()
        };
        self.last_used_profile_id = Some(profile.id.clone());

        let user_message = match self
            .append_message(
                session_id,
                ChatRole::User,
                message,
                Some(profile.id.clone()),
            )
            .await
        {
            Ok(appended) => appended,
            Err(error) => {
                self.clear_active_turn(session_id);
                return Err(error);
            }
        };
        let user_msg_id = user_message.message.id.clone();
        let context = match self
            .get_model_history(&user_msg_id, (&provider_id, &model_id))
            .await
        {
            Ok(context) => context,
            Err(error) => {
                self.clear_active_turn(session_id);
                if let Err(rollback_error) = self
                    .conversation_store
                    .restore_appended_message(session_id, &user_message)
                    .await
                {
                    tracing::error!(
                        "Failed to roll back user message {} after context preparation failure: {rollback_error}",
                        user_msg_id
                    );
                }
                return Err(error);
            }
        };
        let max_context = self
            .resolve_model_max_context(&provider_id, &model_id)
            .await;
        let prepared = async {
            let prompt = self
                .build_turn_system_prompt(
                    session_id,
                    &profile,
                    &workspace_dir,
                    script_tool_transport,
                )
                .await?;
            let (request, compaction) = self
                .build_model_context(
                    session_id,
                    context,
                    &prompt,
                    script_tool_definition(script_tool_transport),
                    max_context,
                    (&provider_id, &model_id),
                )
                .await?;
            Ok::<_, color_eyre::Report>((prompt, request, compaction))
        }
        .await;
        let (system_prompt, provider_request, context_compaction) = match prepared {
            Ok(system_prompt) => system_prompt,
            Err(error) => {
                self.clear_active_turn(session_id);
                if let Err(rollback_error) = self
                    .conversation_store
                    .restore_appended_message(session_id, &user_message)
                    .await
                {
                    tracing::error!(
                        "Failed to roll back user message {} after system prompt failure: {rollback_error}",
                        user_msg_id
                    );
                }
                return Err(error);
            }
        };
        let stream_id = StreamId::new(Ulid::generate());
        let generation = Some(MessageGeneration {
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            max_context,
            usage: None,
        });
        let assistant_msg_id = match self
            .start_streaming_message(
                session_id,
                ChatRole::Assistant,
                stream_id,
                Some(profile.id.clone()),
                generation,
            )
            .await
        {
            Ok(message_id) => message_id,
            Err(error) => {
                self.clear_active_turn(session_id);
                if let Err(rollback_error) = self
                    .conversation_store
                    .restore_appended_message(session_id, &user_message)
                    .await
                {
                    tracing::error!(
                        "Failed to roll back user message {} after assistant placeholder failure: {rollback_error}",
                        user_msg_id
                    );
                }
                return Err(error);
            }
        };

        Ok(PendingStreamRequest {
            message_id: assistant_msg_id,
            provider_id,
            model_id,
            provider_request,
            context_compaction,
            script_tool_transport,
            context_notifications: system_prompt.context_notifications,
        })
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
        let script_tool_transport = self
            .providers
            .script_tool_transport(&provider_id, &model_id)?;
        let system_prompt = self
            .build_turn_system_prompt(session_id, &profile, &workspace_dir, script_tool_transport)
            .await?;

        let max_context = self
            .resolve_model_max_context(&provider_id, &model_id)
            .await;
        let (provider_request, context_compaction) = self
            .build_model_context(
                session_id,
                context,
                &system_prompt,
                script_tool_definition(script_tool_transport),
                max_context,
                (&provider_id, &model_id),
            )
            .await?;

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
            script_tool_transport,
            context_notifications: system_prompt.context_notifications,
        }))
    }
}

fn script_tool_definition(transport: ScriptToolTransport) -> Option<ScriptToolDefinition> {
    (transport == ScriptToolTransport::NativeCustom).then(|| ScriptToolDefinition {
        name: SCRIPT_TOOL_NAME.to_string(),
        description: SCRIPT_TOOL_DESCRIPTION.to_string(),
    })
}
