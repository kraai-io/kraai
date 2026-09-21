use std::collections::HashSet;
use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use kraai_types::{ConversationItem, Message, MessageGeneration, MessageId, MessageStatus};
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
        let mut session = self
            .session_store
            .get(&request.session_id)
            .await?
            .ok_or_else(|| eyre!("Session not found: {}", request.session_id))?;
        let previous_tip = session.tip_id.clone();
        let previous_title = session.title.clone();
        let message = Message {
            id: message_id.clone(),
            parent_id: previous_tip.clone(),
            content: request.content,
            status: request.status,
            agent_profile_id: request.agent_profile_id,
            generation: request.generation,
        };

        self.message_store.save(&message).await?;

        session.tip_id = Some(message_id.clone());
        if previous_tip.is_none()
            && session.title.is_none()
            && let Some(title) = request.title_if_first_message
        {
            session.title = Some(title);
        }
        session.updated_at = current_unix_timestamp();

        match self
            .session_store
            .save_if_tip_matches(&session, previous_tip.as_ref())
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                self.delete_unreferenced_message(&message_id).await;
                return Err(eyre!(
                    "Session {} changed while appending message {message_id}",
                    request.session_id
                ));
            }
            Err(error) => return Err(error),
        }

        Ok(AppendedMessage {
            message,
            previous_tip,
            previous_title,
        })
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
        mut cursor: Option<MessageId>,
        target: &MessageId,
    ) -> Result<bool> {
        let mut visited = HashSet::new();
        while let Some(id) = cursor {
            if &id == target {
                return Ok(true);
            }
            if !visited.insert(id.clone()) {
                return Err(eyre!(
                    "Corrupt message parent graph: cycle repeats message {id}"
                ));
            }
            let Some(message) = self.message_store.get(&id).await? else {
                return Err(eyre!("History references missing message {id}"));
            };
            cursor = message.parent_id;
        }
        Ok(false)
    }

    pub async fn restore_appended_message(
        &self,
        session_id: &str,
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
        session_id: &str,
        message_id: &MessageId,
        tip_id: Option<MessageId>,
        title: Option<String>,
    ) -> Result<()> {
        let guard = self.session_store.lock_message_mutation(message_id).await;
        let store = self.clone();
        let session_id = session_id.to_string();
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
        session_id: &str,
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

pub struct AppendMessageRequest {
    pub session_id: String,
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

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "turn persistence tests use direct assertions for fixture and failure-path setup"
)]
mod tests;
