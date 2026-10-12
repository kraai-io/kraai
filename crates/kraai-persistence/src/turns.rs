use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use kraai_types::{
    ConversationItem, Message, MessageGeneration, MessageId, MessageStatus, SessionId,
};
use ulid::Ulid;

use crate::commit::complete_commit;
use crate::{MessageStore, SessionStore};

fn current_unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs()
}

#[derive(Clone)]
pub struct ConversationStore {
    message_store: Arc<dyn MessageStore>,
    session_store: Arc<dyn SessionStore>,
}

impl ConversationStore {
    pub fn new(message_store: Arc<dyn MessageStore>, session_store: Arc<dyn SessionStore>) -> Self {
        Self {
            message_store,
            session_store,
        }
    }

    pub async fn append_message(&self, request: AppendMessageRequest) -> Result<AppendedMessage> {
        self.append_new_message(MessageId::new(Ulid::generate()), request)
            .await
    }

    async fn append_new_message(
        &self,
        message_id: MessageId,
        request: AppendMessageRequest,
    ) -> Result<AppendedMessage> {
        let session = self
            .session_store
            .get(&request.session_id)
            .await?
            .ok_or_else(|| eyre!("Session not found: {}", request.session_id))?;
        let appended = self
            .append_messages(
                &request.session_id,
                session.tip_id.as_ref(),
                vec![NewMessage {
                    id: message_id,
                    content: request.content,
                    status: request.status,
                    agent_profile_id: request.agent_profile_id,
                    generation: request.generation,
                }],
                request.title_if_first_message,
                None,
            )
            .await?;
        Ok(AppendedMessage {
            message: appended
                .messages
                .into_iter()
                .next()
                .ok_or_else(|| eyre!("Appending a message linked nothing"))?,
            previous_tip: appended.previous_tip,
            previous_title: appended.previous_title,
        })
    }

    /// Links `messages` after `expected_tip` in one durable step, or links none of them.
    pub async fn append_messages(
        &self,
        session_id: &SessionId,
        expected_tip: Option<&MessageId>,
        messages: Vec<NewMessage>,
        title_if_first_message: Option<String>,
        consumed_queue_through: Option<i64>,
    ) -> Result<AppendedMessages> {
        let mut session = self
            .session_store
            .get(session_id)
            .await?
            .ok_or_else(|| eyre!("Session not found: {session_id}"))?;
        let previous_tip = session.tip_id.clone();
        let previous_title = session.title.clone();
        if previous_tip.as_ref() != expected_tip {
            return Err(eyre!(
                "Session {session_id} changed before its messages could be appended"
            ));
        }
        let mut parent_id = previous_tip.clone();
        let messages: Vec<_> = messages
            .into_iter()
            .map(|message| Message {
                parent_id: parent_id.replace(message.id.clone()),
                id: message.id,
                content: message.content,
                status: message.status,
                agent_profile_id: message.agent_profile_id,
                generation: message.generation,
            })
            .collect();
        let Some(last) = messages.last() else {
            return Err(eyre!("No messages to append to session {session_id}"));
        };
        session.tip_id = Some(last.id.clone());
        if let Some(generation) = messages
            .iter()
            .rev()
            .find_map(|message| message.generation.as_ref())
        {
            session.selected_model = Some(kraai_types::ModelSelection {
                model_id: generation.model_id.clone(),
                provider_id: generation.provider_id.clone(),
                options: generation.options.clone(),
            });
        }
        if previous_tip.is_none()
            && session.title.is_none()
            && let Some(title) = title_if_first_message
        {
            session.title = Some(title);
        }
        session.updated_at = current_unix_timestamp();

        let linked = match self
            .message_store
            .save_linked(crate::LinkRequest {
                messages: &messages,
                session: &session,
                expected_tip: previous_tip.as_ref(),
                consumed_queue_through,
            })
            .await?
        {
            Some(linked) => linked,
            None => {
                self.link_individually(
                    &messages,
                    &session,
                    previous_tip.as_ref(),
                    consumed_queue_through,
                )
                .await?
            }
        };
        if !linked {
            for message in &messages {
                self.delete_unreferenced_message(&message.id).await;
            }
            return Err(eyre!(
                "Session {session_id} changed while appending {} message(s)",
                messages.len()
            ));
        }

        Ok(AppendedMessages {
            messages,
            previous_tip,
            previous_title,
            title: session.title,
        })
    }

    async fn link_individually(
        &self,
        messages: &[Message],
        session: &crate::SessionMeta,
        expected_tip: Option<&MessageId>,
        consumed_queue_through: Option<i64>,
    ) -> Result<bool> {
        for message in messages {
            self.message_store.save(message).await?;
        }
        if !self
            .session_store
            .save_if_tip_matches(session, expected_tip)
            .await?
        {
            return Ok(false);
        }
        if let Some(through) = consumed_queue_through {
            self.session_store
                .remove_queued_messages(&session.id, through)
                .await?;
        }
        Ok(true)
    }

    /// Append a message with a stable ID, or finish linking the same durable message after a
    /// process interruption. This is used for script results whose delivery must be retry-safe.
    pub async fn append_message_idempotent(
        &self,
        message_id: MessageId,
        request: AppendMessageRequest,
    ) -> Result<IdempotentAppendOutcome> {
        let guard = self.session_store.lock_message_mutation(&message_id).await;
        let store = self.clone();
        complete_commit(
            guard,
            async move { store.append_idempotent_message(message_id, request).await },
            "Idempotent append task failed",
        )
        .await
    }

    async fn append_idempotent_message(
        &self,
        message_id: MessageId,
        request: AppendMessageRequest,
    ) -> Result<IdempotentAppendOutcome> {
        let Some(existing) = self.message_store.get(&message_id).await? else {
            let appended = self.append_new_message(message_id, request).await?;
            return Ok(IdempotentAppendOutcome {
                message: appended.message,
                linked_now: true,
            });
        };
        validate_idempotent_message(&existing, &request)?;

        let mut session = self
            .session_store
            .get(&request.session_id)
            .await?
            .ok_or_else(|| eyre!("Session not found: {}", request.session_id))?;
        if session.tip_id.as_ref() == Some(&message_id)
            || self
                .message_is_in_history(session.tip_id.clone(), &message_id)
                .await?
        {
            return Ok(IdempotentAppendOutcome {
                message: existing,
                linked_now: false,
            });
        }
        if session.tip_id != existing.parent_id {
            return Err(eyre!(
                "Cannot recover message {message_id} for session {}: current tip {:?} does not match durable parent {:?}",
                request.session_id,
                session.tip_id,
                existing.parent_id
            ));
        }

        let previous_tip = session.tip_id.clone();
        session.tip_id = Some(message_id.clone());
        session.updated_at = current_unix_timestamp();
        if !self
            .session_store
            .link_message_if_tip_matches(&session, previous_tip.as_ref())
            .await?
        {
            return Err(eyre!(
                "Session {} changed while recovering message {message_id}",
                request.session_id
            ));
        }
        Ok(IdempotentAppendOutcome {
            message: existing,
            linked_now: true,
        })
    }

    async fn message_is_in_history(
        &self,
        tip: Option<MessageId>,
        target: &MessageId,
    ) -> Result<bool> {
        let mut history = crate::HistoryCursor::new(self.message_store.as_ref(), tip);
        while let Some(page) = history.next_page().await? {
            if page.iter().any(|message| &message.id == target) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn restore_appended_message(
        &self,
        session_id: &SessionId,
        appended: &AppendedMessage,
    ) -> Result<()> {
        self.restore_tip_title_and_delete_message(
            session_id,
            &appended.message.id,
            appended.previous_tip.clone(),
            appended.previous_title.clone(),
        )
        .await
    }

    pub async fn restore_tip_title_and_delete_message(
        &self,
        session_id: &SessionId,
        message_id: &MessageId,
        tip_id: Option<MessageId>,
        title: Option<String>,
    ) -> Result<()> {
        let guard = self.session_store.lock_message_mutation(message_id).await;
        let store = self.clone();
        let session_id = session_id.clone();
        let message_id = message_id.clone();
        complete_commit(
            guard,
            async move {
                store
                    .restore_message(&session_id, &message_id, tip_id, title)
                    .await
            },
            "Message rollback task failed",
        )
        .await
    }

    async fn restore_message(
        &self,
        session_id: &SessionId,
        message_id: &MessageId,
        tip_id: Option<MessageId>,
        title: Option<String>,
    ) -> Result<()> {
        let mut session = self
            .session_store
            .get(session_id)
            .await?
            .ok_or_else(|| eyre!("Session not found: {session_id}"))?;
        if session.tip_id.as_ref() != Some(message_id) {
            return Err(eyre!(
                "Cannot restore session {session_id}: tip is not abandoned message {message_id}"
            ));
        }

        session.tip_id = tip_id;
        session.title = title;
        session.updated_at = current_unix_timestamp();
        if !self
            .session_store
            .save_if_tip_matches(&session, Some(message_id))
            .await?
        {
            return Err(eyre!(
                "Cannot restore session {session_id}: tip changed while restoring message {message_id}"
            ));
        }
        // The session no longer references the abandoned message. Cleanup failure should leave an
        // orphan for later GC, not make callers believe the rollback itself failed.
        if let Err(error) = self
            .session_store
            .delete_message_if_unreferenced(message_id, self.message_store.clone())
            .await
        {
            tracing::error!(
                "Failed to delete abandoned message {message_id} after restoring session {session_id}: {error}"
            );
        }
        Ok(())
    }

    async fn delete_unreferenced_message(&self, message_id: &MessageId) {
        if let Err(delete_error) = self
            .session_store
            .delete_message_if_unreferenced(message_id, self.message_store.clone())
            .await
        {
            tracing::error!(
                "Failed to delete unreferenced appended message {message_id}: {delete_error}"
            );
        }
    }
}

#[derive(Clone)]
pub struct AppendMessageRequest {
    pub session_id: SessionId,
    pub content: ConversationItem,
    pub status: MessageStatus,
    pub agent_profile_id: Option<String>,
    pub generation: Option<MessageGeneration>,
    pub title_if_first_message: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AppendedMessage {
    pub message: Message,
    pub previous_tip: Option<MessageId>,
    pub previous_title: Option<String>,
}

#[derive(Clone, Debug)]
pub struct IdempotentAppendOutcome {
    pub message: Message,
    pub linked_now: bool,
}

fn validate_idempotent_message(existing: &Message, request: &AppendMessageRequest) -> Result<()> {
    if existing.content != request.content
        || existing.status != request.status
        || existing.agent_profile_id != request.agent_profile_id
        || existing.generation != request.generation
    {
        return Err(eyre!(
            "Message {} already exists with content that does not match the idempotent append",
            existing.id
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct NewMessage {
    pub id: MessageId,
    pub content: ConversationItem,
    pub status: MessageStatus,
    pub agent_profile_id: Option<String>,
    pub generation: Option<MessageGeneration>,
}

#[derive(Clone, Debug)]
pub struct AppendedMessages {
    pub messages: Vec<Message>,
    pub previous_tip: Option<MessageId>,
    pub previous_title: Option<String>,
    pub title: Option<String>,
}
