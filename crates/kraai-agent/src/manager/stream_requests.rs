use super::*;

/// User messages that are linked together with the assistant message of the turn they start.
pub(super) struct TurnInput {
    pub(super) tip: Option<MessageId>,
    pub(super) users: Vec<kraai_persistence::NewMessage>,
    pub(super) title: Option<String>,
    pub(super) consumed_queue_through: Option<i64>,
}

impl AgentManager {
    pub async fn prepare_start_stream(
        &self,
        session_id: &SessionId,
        message: kraai_types::MessageContent,
        model_id: ModelId,
        provider_id: ProviderId,
        options: ModelOptionValues,
    ) -> AgentResult<PendingStreamRequest> {
        if self.is_turn_active(session_id) {
            return Err(AgentError::conflict(
                "Cannot send a new message while the current turn is active",
            ));
        }
        self.prepare_messages_stream(session_id, vec![message], model_id, provider_id, options)
            .await?
            .ok_or_else(|| eyre!("Failed to prepare a new message stream").into())
    }

    pub async fn prepare_continuation_stream(
        &self,
        session_id: &SessionId,
    ) -> AgentResult<Option<PendingStreamRequest>> {
        self.prepare_turn_stream(session_id, Vec::new(), None).await
    }

    pub(super) async fn prepare_turn_stream(
        &self,
        session_id: &SessionId,
        users: Vec<kraai_types::MessageContent>,
        consumed_queue_through: Option<i64>,
    ) -> AgentResult<Option<PendingStreamRequest>> {
        let selection = lock(&self.session_states)
            .get(session_id)
            .and_then(|state| {
                Some((
                    state.last_model.clone()?,
                    state.last_provider.clone()?,
                    state.last_options.clone(),
                ))
            });
        let Some((model_id, provider_id, options)) = selection else {
            return Ok(None);
        };
        self.validate_model_options(&provider_id, &model_id, &options, true)
            .await?;
        let session = self
            .recover_interrupted_stream(self.require_session(session_id).await?)
            .await?;
        let selected_profile = self.resolve_selected_profile(&session)?;
        let (profile, workspace_dir) =
            self.with_runtime_state(session_id, &session.workspace_dir, |state| {
                let profile = match state.active_turn_profile.clone() {
                    Some(profile) => profile,
                    None => {
                        let selected_profile = Arc::new(selected_profile);
                        state.active_turn_profile = Some(selected_profile.clone());
                        selected_profile
                    }
                };
                (profile, state.active_workspace_dir.clone())
            });

        if self.session_has_active_stream(session_id).await {
            return Ok(None);
        }

        let tip = self.get_tip(session_id).await?;
        let input = TurnInput {
            title: users
                .first()
                .and_then(|content| title_from_user_prompt(&content.display_text())),
            users: users
                .into_iter()
                .map(|content| kraai_persistence::NewMessage {
                    id: MessageId::new(Ulid::generate()),
                    content: ConversationItem::User { content },
                    status: MessageStatus::Complete,
                    agent_profile_id: Some(profile.id.clone()),
                    generation: None,
                })
                .collect(),
            tip,
            consumed_queue_through,
        };
        let mut parent_id = input.tip.clone();
        let pending_users: Vec<_> = input
            .users
            .iter()
            .map(|message| Message {
                parent_id: parent_id.replace(message.id.clone()),
                id: message.id.clone(),
                content: message.content.clone(),
                status: message.status.clone(),
                agent_profile_id: message.agent_profile_id.clone(),
                generation: None,
            })
            .collect();
        if parent_id.is_none() {
            return Ok(None);
        }

        let selection = kraai_types::ModelSelection {
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            options: options.clone(),
        };
        self.persist_model_selection(session_id, &selection).await?;
        let mut context = match &input.tip {
            Some(tip) => {
                self.get_model_history(tip, (&provider_id, &model_id), !pending_users.is_empty())
                    .await?
            }
            None => Vec::new(),
        };
        context.extend(pending_users);
        read(&self.providers)
            .get_provider(&provider_id)
            .ok_or_else(|| {
                kraai_provider_core::ProviderError::ProviderNotFound(provider_id.clone())
            })?;
        let mut system_prompt = self
            .build_turn_system_prompt(session_id, &profile, &workspace_dir)
            .await?;

        let max_context = self
            .resolve_model_max_context(&provider_id, &model_id)
            .await;
        let model_context::ModelContext {
            request: provider_request,
            compaction: context_compaction,
            compact_first,
            notifications,
        } = self
            .build_model_context(
                session_id,
                context,
                &system_prompt,
                Some(ScriptToolDefinition::nushell()),
                max_context,
                &selection,
            )
            .await?;

        system_prompt.context_notifications.extend(notifications);
        let stream_id = StreamId::new(Ulid::generate());
        let generation = Some(MessageGeneration {
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            options,
            max_context,
            usage: None,
        });
        let assistant_msg_id = self
            .start_streaming_message(
                session_id,
                input,
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
            compact_first,
            context_notifications: system_prompt.context_notifications,
        }))
    }
}
