use super::*;
use crate::compaction::{ContextCompaction, assemble};

impl AgentManager {
    pub(super) async fn get_model_history(
        &self,
        from: &MessageId,
        identity: (&ProviderId, &ModelId),
    ) -> Result<Vec<Message>> {
        let store = self.persistence.compactions().clone();
        let mut cursor = Some(from.clone());
        let mut visited = HashSet::new();
        let mut history = Vec::new();
        let mut covered = false;
        let mut found_user = false;
        while let Some(id) = cursor {
            if !visited.insert(id.clone()) {
                return Err(eyre!("Cycle in model context history at {id}"));
            }
            let message = self
                .message_store
                .get(&id)
                .await?
                .ok_or_else(|| eyre!("Missing model context message {id}"))?;
            cursor = message.parent_id.clone();
            let user = matches!(message.content, ConversationItem::User { .. });
            if !covered || (!found_user && user) {
                history.push(message);
            }
            found_user |= user;
            if !covered {
                covered = store
                    .get(&id)
                    .await?
                    .is_some_and(|checkpoint| checkpoint.compatible_with(identity.0, identity.1));
            }
            if covered && found_user {
                break;
            }
        }
        history.reverse();
        Ok(history)
    }

    pub(super) async fn build_model_context(
        &self,
        session_id: &str,
        history: Vec<Message>,
        prompt: &prompts::TurnSystemPrompt,
        script_tool: Option<ScriptToolDefinition>,
        max_context: Option<usize>,
        selection: &kraai_types::ModelSelection,
    ) -> Result<(ProviderRequest, Option<ContextCompaction>, Vec<String>)> {
        let store = self.persistence.compactions().clone();
        let mut previous = None;
        let mut start = 0;
        for (index, message) in history.iter().enumerate().rev() {
            if let Some(checkpoint) = store.get(&message.id).await?
                && checkpoint.compatible_with(&selection.provider_id, &selection.model_id)
            {
                previous = Some(checkpoint);
                start = index + 1;
                break;
            }
        }
        let latest_user = history
            .iter()
            .enumerate()
            .rev()
            .find(|(_, message)| matches!(message.content, ConversationItem::User { .. }));
        let pinned_user = latest_user
            .filter(|(index, _)| *index < start)
            .map(|(_, message)| match &message.content {
                ConversationItem::User { content } => ConversationItem::User {
                    content: content.without_images(),
                },
                other => other.clone(),
            });
        let latest = history
            .last()
            .map(|message| message.id.clone())
            .ok_or_else(|| eyre!("Cannot prepare file context without a conversation message"))?;
        let history: Vec<_> = history.into_iter().skip(start).collect();
        let files = crate::context_state::refresh_context_state(
            self.context_state_store.as_ref(),
            session_id,
            &latest,
            &history,
            previous
                .as_ref()
                .map(|checkpoint| &checkpoint.covered_through),
        )
        .await?;
        let superseded_usage: HashSet<_> = previous
            .iter()
            .flat_map(|checkpoint| &checkpoint.superseded_usage)
            .collect();
        let used_context_tokens = history
            .iter()
            .rev()
            .filter(|message| !superseded_usage.contains(&message.id))
            .find_map(snapshot::message_context_usage)
            .map(|context| context.usage.used_context_tokens());
        let covered_through = history.last().map(|message| message.id.clone());
        let mut request = assemble(
            &prompt.prefix,
            &files.snapshots,
            previous.as_ref(),
            history,
            script_tool,
            &selection.options,
        );
        if let Some(user) = &pinned_user
            && !previous.as_ref().is_some_and(|p| {
                p.replacement
                    .iter()
                    .any(|item| matches!(item, ConversationItem::User { .. }))
            })
        {
            request.messages.insert(1, user.clone());
        }
        crate::context_state::append_notifications(&mut request, &files.notifications);
        let compaction = max_context
            .filter(|limit| {
                *limit > 0
                    && used_context_tokens
                        .is_some_and(|used| (used as u128) * 100 >= (*limit as u128) * 80)
            })
            .map(|_| ContextCompaction {
                store,
                usage_store: self.usage_store.clone(),
                session_id: session_id.to_string(),
                original: request.clone(),
                prefix: prompt.prefix.clone(),
                snapshots: files.snapshots,
                file_notifications: files.notifications.clone(),
                covered_through,
                previous_boundary: previous.map(|checkpoint| checkpoint.covered_through),
                on_usage: None,
                usage_barrier: None,
                image_resolver: None,
            });
        Ok((request, compaction, files.notifications))
    }
}
