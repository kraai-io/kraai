use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use kraai_types::{AssistantItem, ChatRole, ConversationItem, Message, MessageId, MessageStatus};

use crate::components::{ChatHistory, RenderedLine};

use super::state::AppState;
use super::types::UiMode;

impl AppState {
    pub(super) fn chat_max_scroll(&self) -> u16 {
        let cache = self.chat_render_cache.borrow();
        cache.total_lines.saturating_sub(self.chat_viewport_height)
    }

    pub(super) fn rendered_messages(&self) -> Vec<Cow<'_, Message>> {
        let mut rendered_messages: Vec<Cow<'_, Message>> =
            build_tip_chain(&self.chat_history, self.current_tip_id.as_deref())
                .into_iter()
                .map(Cow::Borrowed)
                .collect();

        for optimistic in &self.optimistic_messages {
            let content = if optimistic.is_queued {
                format!("{} [queued]", optimistic.content)
            } else {
                optimistic.content.clone()
            };
            rendered_messages.push(Cow::Owned(Message {
                id: MessageId::new(optimistic.local_id.clone()),
                parent_id: None,
                content: ConversationItem::User { text: content },
                status: MessageStatus::Complete,
                agent_profile_id: self.selected_profile_id.clone(),
                generation: None,
            }));
        }

        rendered_messages
    }

    pub(super) fn refresh_chat_render_cache(&self, width: u16) {
        let needs_refresh = {
            let cache = self.chat_render_cache.borrow();
            cache.epoch != self.chat_epoch || cache.width != width
        };
        if !needs_refresh {
            return;
        }

        let rendered_messages = self.rendered_messages();
        let completed: HashSet<&str> = rendered_messages
            .iter()
            .filter_map(|message| match &message.content {
                ConversationItem::ScriptResult { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        let mut sources = HashMap::new();
        for message in &rendered_messages {
            if let ConversationItem::Assistant { items } = &message.content {
                for item in items {
                    if let AssistantItem::ScriptCall { call_id, input, .. } = item
                        && completed.contains(call_id.as_str())
                    {
                        sources.insert(call_id.as_str(), input.as_str());
                    }
                }
            }
        }
        let mut cache = self.chat_render_cache.borrow_mut();
        let mut prior_entries = std::mem::take(&mut cache.message_cache);
        if cache.width != width {
            prior_entries.clear();
        }

        let mut next_entries: HashMap<String, CachedMessageRender> = HashMap::new();
        let mut sections = Vec::new();
        let mut total_lines: u16 = 0;
        let mut execution_offsets = HashMap::new();

        for msg in &rendered_messages {
            if self.mode == UiMode::Executions
                && !matches!(msg.content, ConversationItem::ScriptResult { .. })
            {
                continue;
            }
            let msg = without_completed_calls(msg, &completed);
            let key = msg.id.as_str().to_string();
            let mut fingerprint = message_fingerprint(&msg);
            let lines = if let ConversationItem::ScriptResult { call_id, output } = &msg.content {
                let expanded = self
                    .execution_expanded
                    .get(call_id.as_str())
                    .copied()
                    .unwrap_or(false);
                execution_offsets.insert(
                    call_id.to_string(),
                    total_lines.saturating_add(u16::from(!sections.is_empty())),
                );
                let source = sources.get(call_id.as_str()).copied();
                let selected = self.selected_execution.as_deref() == Some(call_id.as_str());
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                (fingerprint, source, expanded, selected).hash(&mut hasher);
                fingerprint = hasher.finish();
                match prior_entries.remove(&key) {
                    Some(entry) if entry.fingerprint == fingerprint => entry.lines,
                    _ => Arc::new(ChatHistory::build_execution_lines(
                        &super::executions::result_summary(output),
                        source,
                        output,
                        expanded,
                        selected,
                        width,
                    )),
                }
            } else if matches!(&msg.content, ConversationItem::Assistant { items } if items.is_empty())
            {
                continue;
            } else {
                match prior_entries.remove(&key) {
                    Some(entry) if entry.fingerprint == fingerprint => entry.lines,
                    _ => Arc::new(ChatHistory::build_message_lines(&msg, width)),
                }
            };

            if lines.is_empty() {
                continue;
            }

            if !sections.is_empty() {
                sections.push(Arc::clone(
                    cache
                        .separator
                        .get_or_insert_with(|| Arc::new(vec![ChatHistory::separator_line()])),
                ));
                total_lines = total_lines.saturating_add(1);
            }

            total_lines = total_lines.saturating_add(lines.len().min(u16::MAX as usize) as u16);
            sections.push(Arc::clone(&lines));
            next_entries.insert(key, CachedMessageRender { fingerprint, lines });
        }

        cache.execution_offsets = execution_offsets;
        cache.sections = sections;
        cache.total_lines = total_lines;
        cache.message_cache = next_entries;
        cache.width = width;
        cache.epoch = self.chat_epoch;
    }
}

#[derive(Default)]
pub(super) struct ChatRenderCache {
    pub(super) execution_offsets: HashMap<String, u16>,
    pub(super) width: u16,
    pub(super) epoch: u64,
    pub(super) sections: Vec<Arc<Vec<RenderedLine>>>,
    pub(super) total_lines: u16,
    message_cache: HashMap<String, CachedMessageRender>,
    separator: Option<Arc<Vec<RenderedLine>>>,
}

struct CachedMessageRender {
    fingerprint: u64,
    lines: Arc<Vec<RenderedLine>>,
}

fn without_completed_calls<'a>(
    message: &'a Message,
    completed: &HashSet<&str>,
) -> Cow<'a, Message> {
    let ConversationItem::Assistant { items } = &message.content else {
        return Cow::Borrowed(message);
    };
    let is_completed = |item: &AssistantItem| {
        matches!(item, AssistantItem::ScriptCall { call_id, .. }
            if completed.contains(call_id.as_str()))
    };
    if !items.iter().any(is_completed) {
        return Cow::Borrowed(message);
    }
    Cow::Owned(Message {
        id: message.id.clone(),
        parent_id: message.parent_id.clone(),
        content: ConversationItem::Assistant {
            items: items
                .iter()
                .filter(|item| !is_completed(item))
                .cloned()
                .collect(),
        },
        status: message.status.clone(),
        agent_profile_id: message.agent_profile_id.clone(),
        generation: message.generation.clone(),
    })
}

pub(super) fn build_tip_chain<'a>(
    history: &'a BTreeMap<MessageId, Message>,
    current_tip_id: Option<&str>,
) -> Vec<&'a Message> {
    if history.is_empty() {
        return Vec::new();
    }

    let mut parent_ids: HashSet<&MessageId> = HashSet::new();
    for msg in history.values() {
        if let Some(parent_id) = &msg.parent_id {
            parent_ids.insert(parent_id);
        }
    }

    let inferred_tip = history
        .keys()
        .find(|id| !parent_ids.contains(*id))
        .map(ToString::to_string);

    let current_tip_is_leaf = current_tip_id.is_some_and(|id| {
        let message_id = MessageId::new(id.to_string());
        history.contains_key(&message_id) && !parent_ids.contains(&message_id)
    });

    let tip_id = if current_tip_is_leaf {
        current_tip_id.map(|id| MessageId::new(id.to_string()))
    } else {
        inferred_tip.map(MessageId::new)
    };

    let mut chain = Vec::new();
    let mut cursor = tip_id;

    while let Some(message_id) = cursor {
        if let Some(message) = history.get(&message_id) {
            chain.push(message);
            cursor = message.parent_id.clone();
        } else {
            break;
        }
    }

    chain.reverse();
    chain
}

fn message_fingerprint(msg: &Message) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    msg.id.as_str().hash(&mut hasher);
    msg.parent_id
        .as_ref()
        .map(|id| id.as_str())
        .hash(&mut hasher);
    match msg.role() {
        ChatRole::System => 0u8,
        ChatRole::User => 1u8,
        ChatRole::Assistant => 2u8,
        ChatRole::ToolCallResult => 3u8,
    }
    .hash(&mut hasher);
    match &msg.status {
        MessageStatus::Complete => 0u8.hash(&mut hasher),
        MessageStatus::Streaming { stream_id } => {
            1u8.hash(&mut hasher);
            stream_id.as_str().hash(&mut hasher);
        }
        MessageStatus::Cancelled => 2u8.hash(&mut hasher),
    }
    match &msg.content {
        ConversationItem::System { text } | ConversationItem::User { text } => {
            text.hash(&mut hasher);
        }
        ConversationItem::ScriptResult { output, .. } => output.hash(&mut hasher),
        ConversationItem::Assistant { .. } => msg.display_text().hash(&mut hasher),
        ConversationItem::Compaction { .. } => {}
    }
    hasher.finish()
}

#[cfg(test)]
#[path = "chat_render_tests.rs"]
mod tests;
