use super::*;
use crate::compaction::{ContextCompaction, assemble, estimate_request, input_limit};
use kraai_persistence::FileCompactionStore;

impl AgentManager {
    pub(super) async fn get_model_history(&self, from: &MessageId) -> Result<Vec<Message>> {
        let store = FileCompactionStore::new(&self.storage_root);
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
                covered = store.get(&id).await?.is_some();
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
    ) -> Result<(ProviderRequest, Option<ContextCompaction>)> {
        let store = FileCompactionStore::new(&self.storage_root);
        let mut previous = None;
        let mut start = 0;
        for (index, message) in history.iter().enumerate().rev() {
            if let Some(checkpoint) = store.get(&message.id).await? {
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
            .map(|(_, message)| message.content.clone());
        let history: Vec<_> = history.into_iter().skip(start).collect();
        let mut request = assemble(
            &prompt.prefix,
            &prompt.suffix,
            previous.as_ref(),
            &history,
            script_tool,
        );
        if let Some(user) = &pinned_user {
            request.messages.insert(1, user.clone());
        }
        let compaction = max_context
            .filter(|limit| {
                let fixed = estimate_request(&assemble(
                    &prompt.prefix,
                    &prompt.suffix,
                    None,
                    &[],
                    request.script_tool.clone(),
                ));
                let available = input_limit(*limit).saturating_sub(fixed);
                estimate_request(&request).saturating_sub(fixed)
                    >= available.saturating_mul(80) / 100
            })
            .map(|max_context| ContextCompaction {
                store,
                usage_store: self.usage_store.clone(),
                session_id: session_id.to_string(),
                original: request.clone(),
                prefix: prompt.prefix.clone(),
                suffix: prompt.suffix.clone(),
                history,
                previous,
                pinned_user,
                max_context,
                on_usage: None,
                usage_barrier: None,
            });
        Ok((request, compaction))
    }
}
