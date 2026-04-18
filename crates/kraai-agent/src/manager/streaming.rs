use super::*;

impl AgentManager {
    pub async fn prepare_start_stream(
        &mut self,
        session_id: &str,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
    ) -> Result<PendingStreamRequest> {
        self.prepare_start_stream_with_options(session_id, message, model_id, provider_id, false)
            .await
    }

    pub async fn prepare_start_stream_with_options(
        &mut self,
        session_id: &str,
        message: String,
        model_id: ModelId,
        provider_id: ProviderId,
        auto_approve: bool,
    ) -> Result<PendingStreamRequest> {
        let session = self.require_session(session_id).await?;
        let profile = self.resolve_selected_profile(&session)?;
        let workspace_dir = {
            let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
            if state.active_turn_profile.is_some() {
                return Err(eyre!(
                    "Cannot send a new message while the current turn is active"
                ));
            }
            state.promote_pending_tool_config();
            state.last_model = Some(model_id.clone());
            state.last_provider = Some(provider_id.clone());
            state.active_turn_profile = Some(profile.clone());
            state.active_turn_auto_approve = auto_approve;
            state.active_tool_config.workspace_dir.clone()
        };
        self.last_used_profile_id = Some(profile.id.clone());

        let user_msg_id = match self
            .add_message(
                session_id,
                ChatRole::User,
                message,
                Some(profile.id.clone()),
            )
            .await
        {
            Ok(message_id) => message_id,
            Err(error) => {
                self.clear_active_turn(session_id);
                return Err(error);
            }
        };
        let context = match self.get_history_context(&user_msg_id).await {
            Ok(context) => context,
            Err(error) => {
                self.clear_active_turn(session_id);
                return Err(error);
            }
        };
        let mut tool_state_snapshot = resolve_snapshot_from_history(&context);
        let system_prompt = self.build_turn_system_prompt(
            session_id,
            &profile,
            &workspace_dir,
            &mut tool_state_snapshot,
        )?;
        if let Some(state) = self.session_states.get_mut(session_id) {
            state.active_turn_tool_state_snapshot = Some(tool_state_snapshot.clone());
        }
        if let Err(error) = self
            .persist_tool_state_snapshot(&user_msg_id, tool_state_snapshot.clone())
            .await
        {
            self.clear_active_turn(session_id);
            return Err(error);
        }

        let mut provider_messages: Vec<ChatMessage> = context
            .into_iter()
            .map(|m| ChatMessage {
                role: m.role,
                content: m.content,
            })
            .collect();

        if !system_prompt.is_empty() {
            provider_messages.push(ChatMessage {
                role: ChatRole::System,
                content: system_prompt,
            });
        }

        let call_id = CallId::new(Ulid::new());
        let generation = Some(MessageGeneration {
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            max_context: self
                .resolve_model_max_context(&provider_id, &model_id)
                .await,
            usage: None,
        });
        let assistant_msg_id = match self
            .start_streaming_message(
                session_id,
                ChatRole::Assistant,
                call_id,
                Some(profile.id),
                generation,
            )
            .await
        {
            Ok(message_id) => message_id,
            Err(error) => {
                self.clear_active_turn(session_id);
                return Err(error);
            }
        };

        Ok(PendingStreamRequest {
            message_id: assistant_msg_id,
            provider_id,
            model_id,
            provider_messages,
        })
    }

    pub async fn prepare_continuation_stream(
        &mut self,
        session_id: &str,
    ) -> Result<Option<PendingStreamRequest>> {
        let session = self.require_session(session_id).await?;
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
                    state.active_turn_profile = Some(selected_profile.clone());
                    state.active_turn_auto_approve = false;
                    selected_profile.clone()
                }
            };
            (
                model_id.clone(),
                provider_id.clone(),
                profile,
                state.active_tool_config.workspace_dir.clone(),
            )
        };

        if self.session_has_active_stream(session_id).await {
            return Ok(None);
        }

        let tip_id = match self.get_tip(session_id).await? {
            Some(id) => id,
            None => return Ok(None),
        };

        let context = self.get_history_context(&tip_id).await?;
        let mut tool_state_snapshot = resolve_snapshot_from_history(&context);
        let system_prompt = self.build_turn_system_prompt(
            session_id,
            &profile,
            &workspace_dir,
            &mut tool_state_snapshot,
        )?;
        if let Some(state) = self.session_states.get_mut(session_id) {
            state.active_turn_tool_state_snapshot = Some(tool_state_snapshot.clone());
        }
        self.persist_tool_state_snapshot(&tip_id, tool_state_snapshot.clone())
            .await?;

        let mut provider_messages: Vec<ChatMessage> = context
            .into_iter()
            .map(|m| ChatMessage {
                role: m.role,
                content: m.content,
            })
            .collect();

        if !system_prompt.is_empty() {
            provider_messages.push(ChatMessage {
                role: ChatRole::System,
                content: system_prompt,
            });
        }

        let call_id = CallId::new(Ulid::new());
        let generation = Some(MessageGeneration {
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            max_context: self
                .resolve_model_max_context(&provider_id, &model_id)
                .await,
            usage: None,
        });
        let assistant_msg_id = self
            .start_streaming_message(
                session_id,
                ChatRole::Assistant,
                call_id,
                Some(profile.id),
                generation,
            )
            .await?;

        Ok(Some(PendingStreamRequest {
            message_id: assistant_msg_id,
            provider_id,
            model_id,
            provider_messages,
        }))
    }

    pub(super) async fn add_message(
        &mut self,
        session_id: &str,
        role: ChatRole,
        content: String,
        agent_profile_id: Option<String>,
    ) -> Result<MessageId> {
        self.add_message_with_tool_state(
            session_id,
            role,
            content,
            agent_profile_id,
            None,
            Vec::new(),
        )
        .await
    }

    pub(super) async fn add_message_with_tool_state(
        &mut self,
        session_id: &str,
        role: ChatRole,
        content: String,
        agent_profile_id: Option<String>,
        tool_state_snapshot: Option<ToolStateSnapshot>,
        tool_state_deltas: Vec<kraai_types::ToolStateDelta>,
    ) -> Result<MessageId> {
        self.require_session(session_id).await?;

        let message_id = MessageId::new(Ulid::new());
        let parent_id = self.get_tip(session_id).await?;
        let session_title = if role == ChatRole::User && parent_id.is_none() {
            title_from_user_prompt(&content)
        } else {
            None
        };

        tracing::debug!(
            "Adding message: session={}, id={}, role={:?}, parent={:?}",
            session_id,
            message_id,
            role,
            parent_id
        );

        let message = Message {
            id: message_id.clone(),
            parent_id,
            role,
            content,
            status: MessageStatus::Complete,
            agent_profile_id,
            tool_state_snapshot,
            tool_state_deltas,
            generation: None,
        };

        self.message_store.save(&message).await?;
        self.set_tip(session_id, Some(message_id.clone())).await?;
        self.maybe_set_title_from_first_user_message(session_id, session_title)
            .await?;

        Ok(message_id)
    }

    pub(super) async fn start_streaming_message(
        &mut self,
        session_id: &str,
        role: ChatRole,
        call_id: CallId,
        agent_profile_id: Option<String>,
        generation: Option<MessageGeneration>,
    ) -> Result<MessageId> {
        self.require_session(session_id).await?;

        if self.session_has_active_stream(session_id).await {
            return Err(eyre!("Session already has an active stream: {session_id}"));
        }

        let message_id = MessageId::new(Ulid::new());
        let previous_tip = self.get_tip(session_id).await?;

        let message = Message {
            id: message_id.clone(),
            parent_id: previous_tip.clone(),
            role,
            content: String::new(),
            status: MessageStatus::Streaming { call_id },
            agent_profile_id,
            tool_state_snapshot: None,
            tool_state_deltas: Vec::new(),
            generation,
        };

        let mut streaming = self.streaming_messages.write().await;
        streaming.insert(
            message_id.clone(),
            StreamingMessageState {
                session_id: session_id.to_string(),
                previous_tip,
                message,
            },
        );
        drop(streaming);

        self.set_tip(session_id, Some(message_id.clone())).await?;
        Ok(message_id)
    }

    pub(super) async fn session_has_active_stream(&self, session_id: &str) -> bool {
        let streaming = self.streaming_messages.read().await;
        streaming
            .values()
            .any(|state| state.session_id == session_id)
    }

    pub async fn append_chunk(&self, message_id: &MessageId, chunk: &str) -> bool {
        let mut streaming = self.streaming_messages.write().await;
        if let Some(state) = streaming.get_mut(message_id) {
            state.message.content.push_str(chunk);
            true
        } else {
            false
        }
    }

    pub async fn set_streaming_message_usage(
        &self,
        message_id: &MessageId,
        usage: TokenUsage,
    ) -> bool {
        let mut streaming = self.streaming_messages.write().await;
        if let Some(state) = streaming.get_mut(message_id)
            && let Some(generation) = state.message.generation.as_mut()
        {
            generation.usage = Some(usage);
            return true;
        }
        false
    }

    pub async fn complete_message(&self, message_id: &MessageId) -> Result<Option<String>> {
        let state = self.streaming_messages.write().await.remove(message_id);
        let Some(mut state) = state else {
            return Ok(None);
        };
        let original_state = state.clone();

        state.message.status = MessageStatus::Complete;
        if let Err(error) = self.message_store.save(&state.message).await {
            self.streaming_messages
                .write()
                .await
                .insert(message_id.clone(), original_state);
            return Err(error);
        }
        Ok(Some(state.session_id))
    }

    pub async fn abort_streaming_message(&self, message_id: &MessageId) -> Result<Option<String>> {
        let state = self.streaming_messages.write().await.remove(message_id);
        let Some(state) = state else {
            return Ok(None);
        };
        let original_state = state.clone();

        if let Err(error) = self
            .set_tip(&state.session_id, state.previous_tip.clone())
            .await
        {
            self.streaming_messages
                .write()
                .await
                .insert(message_id.clone(), original_state);
            return Err(error);
        }
        Ok(Some(state.session_id))
    }

    pub async fn cancel_streaming_message(
        &self,
        message_id: &MessageId,
    ) -> Result<Option<CancelledStreamResult>> {
        let state = self.streaming_messages.write().await.remove(message_id);
        let Some(mut state) = state else {
            return Ok(None);
        };

        let persisted = !state.message.content.is_empty();
        if persisted {
            state.message.status = MessageStatus::Complete;
            self.message_store.save(&state.message).await?;
        } else {
            self.set_tip(&state.session_id, state.previous_tip).await?;
        }

        Ok(Some(CancelledStreamResult {
            session_id: state.session_id,
            message_id: message_id.clone(),
            persisted,
        }))
    }

    pub(super) async fn abort_streaming_messages_for_session(
        &self,
        session_id: &str,
    ) -> Result<()> {
        let to_abort: Vec<MessageId> = {
            let streaming = self.streaming_messages.read().await;
            streaming
                .iter()
                .filter_map(|(message_id, state)| {
                    (state.session_id == session_id).then_some(message_id.clone())
                })
                .collect()
        };

        for message_id in to_abort {
            self.abort_streaming_message(&message_id).await?;
        }

        Ok(())
    }

    async fn get_history_context(&self, from: &MessageId) -> Result<Vec<Message>> {
        let mut context = Vec::new();
        let mut current = Some(from.clone());

        while let Some(id) = current {
            {
                let streaming = self.streaming_messages.read().await;
                if let Some(state) = streaming.get(&id) {
                    context.push(state.message.clone());
                    current = state.message.parent_id.clone();
                    continue;
                }
            }

            if let Some(msg) = self.message_store.get(&id).await? {
                context.push(msg.clone());
                current = msg.parent_id.clone();
            } else {
                break;
            }
        }

        context.reverse();
        Ok(context)
    }

    pub async fn get_chat_history(&self, session_id: &str) -> Result<BTreeMap<MessageId, Message>> {
        let mut result = BTreeMap::new();

        let tip_id = match self.get_tip(session_id).await? {
            Some(id) => id,
            None => return Ok(result),
        };

        let context = self.get_history_context(&tip_id).await?;
        for msg in context {
            result.insert(msg.id.clone(), msg);
        }

        let streaming = self.streaming_messages.read().await;
        for (message_id, state) in streaming.iter() {
            if state.session_id == session_id {
                result.insert(message_id.clone(), state.message.clone());
            }
        }

        Ok(result)
    }

    pub async fn get_session_context_usage(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionContextUsage>> {
        let Some(tip_id) = self.get_tip(session_id).await? else {
            return Ok(None);
        };

        let context = self.get_history_context(&tip_id).await?;
        Ok(context.into_iter().rev().find_map(|message| {
            (message.role == ChatRole::Assistant && message.status == MessageStatus::Complete)
                .then_some(message.generation)
                .flatten()
                .and_then(|generation| {
                    generation.usage.map(|usage| SessionContextUsage {
                        provider_id: generation.provider_id,
                        model_id: generation.model_id,
                        max_context: generation.max_context,
                        usage,
                    })
                })
        }))
    }

    pub async fn undo_last_user_message(&self, session_id: &str) -> Result<Option<String>> {
        if self.is_turn_active(session_id) {
            return Err(eyre!("Cannot undo while the current turn is active"));
        }

        let Some(mut cursor) = self.get_tip(session_id).await? else {
            return Ok(None);
        };

        let history = self.get_chat_history(session_id).await?;
        while let Some(message) = history.get(&cursor) {
            if message.role == ChatRole::User {
                self.set_tip(session_id, message.parent_id.clone()).await?;
                return Ok(Some(message.content.clone()));
            }

            let Some(parent_id) = message.parent_id.clone() else {
                break;
            };
            cursor = parent_id;
        }

        Ok(None)
    }
}
