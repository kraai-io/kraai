use super::*;

impl AgentManager {
    #[cfg(test)]
    pub(super) async fn add_message(
        &mut self,
        session_id: &str,
        role: ChatRole,
        content: kraai_types::MessageContent,
        agent_profile_id: Option<String>,
    ) -> Result<MessageId> {
        Ok(self
            .append_message(session_id, role, content, agent_profile_id)
            .await?
            .message
            .id)
    }

    pub async fn add_script_result_to_history(
        &mut self,
        session_id: &str,
        message_id: MessageId,
        profile_id: String,
        call_id: ToolCallId,
        content: kraai_types::MessageContent,
    ) -> Result<bool> {
        self.require_session(session_id).await?;
        let outcome = self
            .conversation_store
            .append_message_idempotent(
                message_id,
                AppendMessageRequest {
                    session_id: session_id.to_string(),
                    content: ConversationItem::ScriptResult {
                        call_id,
                        output: content,
                    },
                    status: MessageStatus::Complete,
                    agent_profile_id: Some(profile_id),
                    generation: None,
                    title_if_first_message: None,
                },
            )
            .await?;
        Ok(outcome.linked_now)
    }

    pub(super) async fn append_message(
        &mut self,
        session_id: &str,
        role: ChatRole,
        content: kraai_types::MessageContent,
        agent_profile_id: Option<String>,
    ) -> Result<AppendedMessage> {
        let title_if_first_message = if role == ChatRole::User {
            title_from_user_prompt(&content.display_text())
        } else {
            None
        };
        let content = match role {
            ChatRole::System => ConversationItem::System {
                text: content.display_text().into_owned(),
            },
            ChatRole::User => ConversationItem::User { content },
            ChatRole::Assistant => ConversationItem::Assistant {
                items: vec![AssistantItem::Text {
                    phase: AssistantPhase::FinalAnswer,
                    text: content.display_text().into_owned(),
                }],
            },
            ChatRole::ToolCallResult => {
                return Err(eyre!(
                    "script results require add_script_result_to_history and a call id"
                ));
            }
        };

        let appended = self
            .conversation_store
            .append_message(AppendMessageRequest {
                session_id: session_id.to_string(),
                content,
                status: MessageStatus::Complete,
                agent_profile_id,
                generation: None,
                title_if_first_message,
            })
            .await?;

        tracing::debug!(
            "Added message: session={}, id={}, role={:?}, parent={:?}",
            session_id,
            appended.message.id,
            appended.message.role(),
            appended.previous_tip
        );

        Ok(appended)
    }

    pub(super) async fn start_streaming_message(
        &mut self,
        session_id: &str,
        role: ChatRole,
        stream_id: StreamId,
        agent_profile_id: Option<String>,
        generation: Option<MessageGeneration>,
    ) -> Result<MessageId> {
        self.require_session(session_id).await?;

        if role != ChatRole::Assistant {
            return Err(eyre!("only assistant messages can stream"));
        }

        if self.session_has_active_stream(session_id).await {
            return Err(eyre!(kraai_types::DomainError::conflict(format!(
                "Session already has an active stream: {session_id}"
            ))));
        }

        let subscription = generation
            .as_ref()
            .is_some_and(|generation| self.providers.is_subscription(&generation.provider_id));
        let appended = self
            .conversation_store
            .append_message(AppendMessageRequest {
                session_id: session_id.to_string(),
                content: ConversationItem::Assistant { items: Vec::new() },
                status: MessageStatus::Streaming { stream_id },
                agent_profile_id,
                generation,
                title_if_first_message: None,
            })
            .await?;
        let message_id = appended.message.id.clone();

        self.streaming_messages.write().await.insert(
            message_id.clone(),
            StreamingMessageState {
                session_id: session_id.to_string(),
                previous_tip: appended.previous_tip,
                previous_title: appended.previous_title,
                message: appended.message,
                cancellation_result_id: None,
                text_item_ids: HashMap::new(),
                subscription,
                unpriced_attempts: 0,
                request_started_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            },
        );
        Ok(message_id)
    }

    pub(super) async fn session_has_active_stream(&self, session_id: &str) -> bool {
        let streaming = self.streaming_messages.read().await;
        streaming
            .values()
            .any(|state| state.session_id == session_id)
    }

    pub async fn append_text_chunk(
        &self,
        message_id: &MessageId,
        item_id: &str,
        phase: AssistantPhase,
        chunk: &str,
    ) -> Option<String> {
        let mut streaming = self.streaming_messages.write().await;
        let state = streaming.get_mut(message_id)?;
        let ConversationItem::Assistant { items } = &mut state.message.content else {
            return None;
        };

        let mut visible = String::new();
        let item_index = if let Some(index) = state.text_item_ids.get(item_id).copied() {
            index
        } else {
            if items
                .iter()
                .any(|item| !matches!(item, AssistantItem::Reasoning { .. }))
                && !chunk.is_empty()
            {
                visible.push_str("\n\n");
            }
            items.push(AssistantItem::Text {
                phase,
                text: String::new(),
            });
            let index = items.len().saturating_sub(1);
            state.text_item_ids.insert(item_id.to_string(), index);
            index
        };
        let Some(AssistantItem::Text {
            phase: stored_phase,
            text,
        }) = items.get_mut(item_index)
        else {
            return None;
        };
        if *stored_phase != phase {
            return None;
        }
        text.push_str(chunk);
        visible.push_str(chunk);
        drop(streaming);
        Some(visible)
    }

    pub async fn append_reasoning(
        &self,
        message_id: &MessageId,
        provider_id: ProviderId,
        payload: serde_json::Value,
    ) -> Option<()> {
        let mut streaming = self.streaming_messages.write().await;
        let state = streaming.get_mut(message_id)?;
        let ConversationItem::Assistant { items } = &mut state.message.content else {
            return None;
        };
        items.push(AssistantItem::Reasoning {
            provider_id,
            payload,
        });
        drop(streaming);
        Some(())
    }

    pub async fn append_script_call(
        &self,
        message_id: &MessageId,
        call_id: ToolCallId,
        name: String,
        input: String,
    ) -> Option<String> {
        let mut streaming = self.streaming_messages.write().await;
        let state = streaming.get_mut(message_id)?;
        let ConversationItem::Assistant { items } = &mut state.message.content else {
            return None;
        };
        if items
            .iter()
            .any(|item| matches!(item, AssistantItem::ScriptCall { .. }))
        {
            return None;
        }
        let separator = if items
            .iter()
            .any(|item| !matches!(item, AssistantItem::Reasoning { .. }))
        {
            "\n\n"
        } else {
            ""
        };
        let visible = format!("{separator}<tool_call>\n{input}\n</tool_call>");
        items.push(AssistantItem::ScriptCall {
            call_id,
            name,
            input,
        });
        drop(streaming);
        Some(visible)
    }

    pub async fn complete_message(&self, message_id: &MessageId) -> Result<Option<String>> {
        let state = self.streaming_messages.write().await.remove(message_id);
        let Some(mut state) = state else {
            return Ok(None);
        };
        let original_status = std::mem::replace(&mut state.message.status, MessageStatus::Complete);
        if let Err(error) = self.message_store.save(&state.message).await {
            state.message.status = original_status;
            self.streaming_messages
                .write()
                .await
                .insert(message_id.clone(), state);
            return Err(error);
        }
        Ok(Some(state.session_id))
    }

    pub async fn abort_streaming_message(&self, message_id: &MessageId) -> Result<Option<String>> {
        let state = self.streaming_messages.write().await.remove(message_id);
        let Some(state) = state else {
            return Ok(None);
        };
        if let Err(error) = self
            .conversation_store
            .restore_tip_title_and_delete_message(
                &state.session_id,
                message_id,
                state.previous_tip.clone(),
                state.previous_title.clone(),
            )
            .await
        {
            self.streaming_messages
                .write()
                .await
                .insert(message_id.clone(), state);
            return Err(error);
        }
        Ok(Some(state.session_id))
    }

    pub async fn cancel_streaming_message(
        &self,
        message_id: &MessageId,
        cancelled_script_output: &str,
    ) -> Result<Option<CancelledStreamResult>> {
        let state = self.streaming_messages.write().await.remove(message_id);
        let Some(mut state) = state else {
            return Ok(None);
        };
        let original_status = state.message.status.clone();

        let persisted = state
            .message
            .content
            .assistant_items()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| !matches!(item, AssistantItem::Reasoning { .. }))
            });
        let persist_result = if persisted {
            async {
                let call_id = state.message.content.assistant_items().and_then(|items| {
                    items.iter().find_map(|item| match item {
                        AssistantItem::ScriptCall { call_id, .. } => Some(call_id.clone()),
                        AssistantItem::Text { .. } | AssistantItem::Reasoning { .. } => None,
                    })
                });
                if let Some(call_id) = call_id {
                    self.message_store.save(&state.message).await?;
                    let result_id = state
                        .cancellation_result_id
                        .get_or_insert_with(|| MessageId::new(Ulid::generate()))
                        .clone();
                    self.conversation_store
                        .append_message_idempotent(
                            result_id,
                            AppendMessageRequest {
                                session_id: state.session_id.clone(),
                                content: ConversationItem::ScriptResult {
                                    call_id,
                                    output: cancelled_script_output.to_string().into(),
                                },
                                status: MessageStatus::Complete,
                                agent_profile_id: state.message.agent_profile_id.clone(),
                                generation: None,
                                title_if_first_message: None,
                            },
                        )
                        .await?;
                }
                state.message.status = MessageStatus::Complete;
                self.message_store.save(&state.message).await
            }
            .await
        } else {
            self.conversation_store
                .restore_tip_title_and_delete_message(
                    &state.session_id,
                    message_id,
                    state.previous_tip.clone(),
                    state.previous_title.clone(),
                )
                .await
        };
        if let Err(error) = persist_result {
            state.message.status = original_status;
            self.streaming_messages
                .write()
                .await
                .insert(message_id.clone(), state);
            return Err(error);
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

    pub async fn get_chat_history(&self, session_id: &str) -> Result<BTreeMap<MessageId, Message>> {
        let mut result = BTreeMap::new();

        let Some(tip_id) = self.get_tip(session_id).await? else {
            return Ok(result);
        };

        self.visit_history_context(&tip_id, |message| {
            result
                .entry(message.id.clone())
                .or_insert_with(|| message.into_owned());
        })
        .await?;

        let streaming = self.streaming_messages.read().await;
        let mut streaming_messages: Vec<_> = streaming
            .iter()
            .filter(|(_, state)| state.session_id == session_id)
            .map(|(message_id, state)| (message_id.clone(), state.message.clone()))
            .collect();
        drop(streaming);
        streaming_messages.sort_by(|(left, _), (right, _)| left.cmp(right));
        result.extend(streaming_messages);

        Ok(result)
    }

    pub async fn undo_last_user_message(
        &self,
        session_id: &str,
    ) -> Result<Option<kraai_types::MessageContent>> {
        if self.pending_message_rollbacks.contains_key(session_id) {
            return Err(eyre!(kraai_types::DomainError::conflict(
                "Cannot undo while queued message rollback is incomplete"
            )));
        }
        if self.is_turn_active(session_id) {
            return Err(eyre!(kraai_types::DomainError::conflict(
                "Cannot undo while the current turn is active"
            )));
        }

        let Some(mut cursor) = self.get_tip(session_id).await? else {
            return Ok(None);
        };

        let history = self.get_chat_history(session_id).await?;
        while let Some(message) = history.get(&cursor) {
            if message.role() == ChatRole::User {
                self.set_tip(session_id, message.parent_id.clone()).await?;
                return Ok(match &message.content {
                    ConversationItem::User { content } => Some(content.clone()),
                    _ => None,
                });
            }

            let Some(parent_id) = message.parent_id.clone() else {
                break;
            };
            cursor = parent_id;
        }

        Ok(None)
    }
}
