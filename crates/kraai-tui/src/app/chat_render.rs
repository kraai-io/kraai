use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use kraai_types::{
    AssistantItem, ChatRole, ConversationItem, Message, MessageId, MessageStatus, ScriptCall,
};

use crate::components::{ChatHistory, RenderedLine};

use super::state::AppState;

impl AppState {
    pub(super) fn rendered_messages(&self) -> Vec<Cow<'_, Message>> {
        let mut rendered_messages: Vec<Cow<'_, Message>> =
            build_tip_chain(&self.chat.history, self.chat.tip_id.as_deref())
                .into_iter()
                .map(Cow::Borrowed)
                .collect();

        for optimistic in &self.chat.optimistic_messages {
            let content = if optimistic.is_queued {
                format!("{} [queued]", optimistic.content)
            } else {
                optimistic.content.clone()
            };
            rendered_messages.push(Cow::Owned(Message {
                id: MessageId::new(optimistic.local_id.clone()),
                parent_id: None,
                content: ConversationItem::User {
                    content: content.into(),
                },
                status: MessageStatus::Complete,
                agent_profile_id: self.agents.selected_profile_id.clone(),
                generation: None,
            }));
        }

        rendered_messages
    }

    pub(super) fn refresh_chat_render_cache(&self, width: u16) {
        let (stale, streamed, cards_stale) = {
            let cache = self.chat.render_cache.borrow();
            (
                cache.epoch != self.chat.epoch || cache.width != width,
                !cache.streamed.is_empty(),
                cache.cards_epoch != self.chat.cards_epoch,
            )
        };
        if stale
            || (streamed && !self.refresh_streamed_messages())
            || (cards_stale && !self.refresh_script_cards())
        {
            self.rebuild_chat_render_cache(width);
        }
    }

    /// False when a streamed message has no section of its own yet or came out empty, which
    /// only a full rebuild can lay out.
    fn refresh_streamed_messages(&self) -> bool {
        let mut cache = self.chat.render_cache.borrow_mut();
        let cache = &mut *cache;
        for id in std::mem::take(&mut cache.streamed) {
            let (Some(&section), Some(message)) =
                (cache.message_sections.get(&id), self.chat.history.get(&id))
            else {
                return false;
            };
            let completed = &cache.completed;
            let live = cache.live.iter_mut().find(|live| live.section == section);
            if !is_visible(message, completed)
                || live.is_some() != has_pending_calls(message, completed)
            {
                return false;
            }
            let base = message_fingerprint(message, completed);
            let fingerprint = self.card_fingerprint(base, message, completed);
            if let Some(live) = live {
                live.base = base;
            }
            let lines = self.build_card_message_lines(message, completed, cache.width);
            if !replace_section(cache, section, id.as_str(), fingerprint, lines) {
                return false;
            }
        }
        true
    }

    /// False when a card came out empty, which only a full rebuild can lay out.
    fn refresh_script_cards(&self) -> bool {
        let mut cache = self.chat.render_cache.borrow_mut();
        let cache = &mut *cache;
        for index in 0..cache.live.len() {
            let Some(live) = cache.live.get(index) else {
                continue;
            };
            let (section, base) = (live.section, live.base);
            let Some(message) = self.chat.history.get(&live.id) else {
                return false;
            };
            let key = live.id.as_str().to_owned();
            let fingerprint = self.card_fingerprint(base, message, &cache.completed);
            if cache
                .message_cache
                .get(&key)
                .is_none_or(|entry| entry.fingerprint == fingerprint)
            {
                continue;
            }
            let lines = self.build_card_message_lines(message, &cache.completed, cache.width);
            if !replace_section(cache, section, &key, fingerprint, lines) {
                return false;
            }
        }
        cache.cards_epoch = self.chat.cards_epoch;
        true
    }

    fn card_fingerprint(&self, base: u64, msg: &Message, completed: &HashSet<String>) -> u64 {
        let ConversationItem::Assistant { items } = &msg.content else {
            return base;
        };
        let writing = matches!(msg.status, MessageStatus::Streaming { .. });
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        base.hash(&mut hasher);
        for item in items {
            if let AssistantItem::ScriptCall(call) = item
                && !completed.contains(call.call_id.as_str())
            {
                self.script_card(call.call_id.as_str(), writing)
                    .hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    fn build_card_message_lines(
        &self,
        msg: &Message,
        completed: &HashSet<String>,
        width: u16,
    ) -> Arc<Vec<RenderedLine>> {
        let writing = matches!(msg.status, MessageStatus::Streaming { .. });
        let card = |call_id: &str| self.script_card(call_id, writing);
        Arc::new(ChatHistory::build_message_lines(
            &without_completed_calls(msg, completed),
            width,
            &card,
        ))
    }

    fn rebuild_chat_render_cache(&self, width: u16) {
        let rendered_messages = self.rendered_messages();
        let completed: HashSet<String> = rendered_messages
            .iter()
            .filter_map(|message| {
                if let ConversationItem::ScriptResult { call_id, .. } = &message.content {
                    Some(call_id.as_str().to_owned())
                } else {
                    None
                }
            })
            .collect();
        let mut sources = HashMap::new();
        for message in &rendered_messages {
            if let ConversationItem::Assistant { items } = &message.content {
                for item in items {
                    if let AssistantItem::ScriptCall(ScriptCall { call_id, input, .. }) = item
                        && completed.contains(call_id.as_str())
                    {
                        sources.insert(call_id.as_str(), input.as_str());
                    }
                }
            }
        }
        let mut cache = self.chat.render_cache.borrow_mut();
        let mut prior_entries = std::mem::take(&mut cache.message_cache);
        if cache.width != width {
            prior_entries.clear();
        }

        let mut next_entries: HashMap<String, CachedMessageRender> = HashMap::new();
        let mut message_sections = HashMap::new();
        let mut sections = Vec::new();
        let mut total_lines = 0_usize;
        let mut live = Vec::new();

        let visible: Vec<&Message> = rendered_messages
            .iter()
            .map(AsRef::as_ref)
            .filter(|msg| is_visible(msg, &completed))
            .collect();
        for group in visible.chunk_by(|left, right| {
            !self.chat.scripts_expanded
                && matches!(left.content, ConversationItem::ScriptResult { .. })
                && matches!(right.content, ConversationItem::ScriptResult { .. })
        }) {
            let Some(&msg) = group.first() else {
                continue;
            };
            let key = msg.id.as_str().to_string();
            let fingerprint;
            let mut live_base = None;
            let grouped = !self.chat.scripts_expanded
                && matches!(msg.content, ConversationItem::ScriptResult { .. });
            let lines = if grouped {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                "execution-group".hash(&mut hasher);
                for message in group {
                    message_fingerprint(message, &completed).hash(&mut hasher);
                }
                fingerprint = hasher.finish();
                match prior_entries.remove(&key) {
                    Some(entry) if entry.fingerprint == fingerprint => entry.lines,
                    _ => Arc::new(ChatHistory::build_script_group_lines(
                        &super::executions::group_summary(group.iter().filter_map(|message| {
                            if let ConversationItem::ScriptResult {
                                outcome, output, ..
                            } = &message.content
                            {
                                Some((*outcome, output.text_only()))
                            } else {
                                None
                            }
                        })),
                        width,
                    )),
                }
            } else if let ConversationItem::ScriptResult {
                call_id,
                output,
                outcome,
            } = &msg.content
            {
                let source = sources.get(call_id.as_str()).copied();
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                (message_fingerprint(msg, &completed), source).hash(&mut hasher);
                fingerprint = hasher.finish();
                match prior_entries.remove(&key) {
                    Some(entry) if entry.fingerprint == fingerprint => entry.lines,
                    _ => {
                        let images = output.image_labels().collect::<Vec<_>>().join("\n");
                        Arc::new(ChatHistory::build_script_lines(
                            &super::executions::script_view(
                                *outcome,
                                &output.text_only(),
                                &images,
                                source,
                            ),
                            width,
                        ))
                    }
                }
            } else {
                let base = message_fingerprint(msg, &completed);
                fingerprint = self.card_fingerprint(base, msg, &completed);
                if has_pending_calls(msg, &completed) {
                    live_base = Some(base);
                }
                match prior_entries.remove(&key) {
                    Some(entry) if entry.fingerprint == fingerprint => entry.lines,
                    _ => self.build_card_message_lines(msg, &completed, width),
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

            total_lines = total_lines.saturating_add(lines.len());
            if let Some(base) = live_base {
                live.push(LiveCards {
                    section: sections.len(),
                    id: msg.id.clone(),
                    base,
                });
            }
            if matches!(msg.content, ConversationItem::Assistant { .. }) {
                message_sections.insert(msg.id.clone(), sections.len());
            }
            sections.push(Arc::clone(&lines));
            next_entries.insert(key, CachedMessageRender { fingerprint, lines });
        }

        cache.sections = sections;
        cache.total_lines = total_lines;
        cache.message_cache = next_entries;
        cache.message_sections = message_sections;
        cache.completed = completed;
        cache.streamed.clear();
        cache.width = width;
        cache.epoch = self.chat.epoch;
        cache.cards_epoch = self.chat.cards_epoch;
        cache.live = live;
    }
}

#[derive(Default)]
pub(super) struct ChatRenderCache {
    pub(super) width: u16,
    pub(super) epoch: u64,
    pub(super) sections: Vec<Arc<Vec<RenderedLine>>>,
    pub(super) total_lines: usize,
    cards_epoch: u64,
    message_cache: HashMap<String, CachedMessageRender>,
    message_sections: HashMap<MessageId, usize>,
    completed: HashSet<String>,
    streamed: BTreeSet<MessageId>,
    separator: Option<Arc<Vec<RenderedLine>>>,
    live: Vec<LiveCards>,
}

impl ChatRenderCache {
    pub(super) fn mark_streamed(&mut self, id: MessageId) {
        self.streamed.insert(id);
    }
}

struct CachedMessageRender {
    fingerprint: u64,
    lines: Arc<Vec<RenderedLine>>,
}

struct LiveCards {
    section: usize,
    id: MessageId,
    base: u64,
}

fn replace_section(
    cache: &mut ChatRenderCache,
    section: usize,
    key: &str,
    fingerprint: u64,
    lines: Arc<Vec<RenderedLine>>,
) -> bool {
    let Some(current) = cache.sections.get_mut(section) else {
        return false;
    };
    if lines.is_empty() {
        return false;
    }
    cache.total_lines = cache
        .total_lines
        .saturating_sub(current.len())
        .saturating_add(lines.len());
    *current = Arc::clone(&lines);
    cache
        .message_cache
        .insert(key.to_owned(), CachedMessageRender { fingerprint, lines });
    true
}

fn is_completed_call(item: &AssistantItem, completed: &HashSet<String>) -> bool {
    matches!(item, AssistantItem::ScriptCall(ScriptCall { call_id, .. })
        if completed.contains(call_id.as_str()))
}

fn is_visible(message: &Message, completed: &HashSet<String>) -> bool {
    let ConversationItem::Assistant { items } = &message.content else {
        return true;
    };
    items.iter().any(|item| match item {
        AssistantItem::Text { text, .. } => !text.trim().is_empty(),
        AssistantItem::ScriptCall(_) => !is_completed_call(item, completed),
        AssistantItem::Reasoning { .. } => false,
    })
}

fn has_pending_calls(message: &Message, completed: &HashSet<String>) -> bool {
    matches!(&message.content, ConversationItem::Assistant { items }
        if items.iter().any(|item| matches!(item, AssistantItem::ScriptCall(_))
            && !is_completed_call(item, completed)))
}

fn without_completed_calls<'a>(
    message: &'a Message,
    completed: &HashSet<String>,
) -> Cow<'a, Message> {
    let ConversationItem::Assistant { items } = &message.content else {
        return Cow::Borrowed(message);
    };
    if !items.iter().any(|item| is_completed_call(item, completed)) {
        return Cow::Borrowed(message);
    }
    Cow::Owned(Message {
        id: message.id.clone(),
        parent_id: message.parent_id.clone(),
        content: ConversationItem::Assistant {
            items: items
                .iter()
                .filter(|item| !is_completed_call(item, completed))
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

fn message_fingerprint(msg: &Message, completed: &HashSet<String>) -> u64 {
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
        ConversationItem::Assistant { items } => {
            for item in items {
                match item {
                    AssistantItem::Text { text, .. } => {
                        0u8.hash(&mut hasher);
                        text.hash(&mut hasher);
                    }
                    AssistantItem::ScriptCall(call) if !is_completed_call(item, completed) => {
                        1u8.hash(&mut hasher);
                        call.call_id.as_str().hash(&mut hasher);
                        call.input.hash(&mut hasher);
                    }
                    AssistantItem::ScriptCall(_) | AssistantItem::Reasoning { .. } => {}
                }
            }
        }
        ConversationItem::ScriptResult { outcome, .. } => {
            msg.display_text().hash(&mut hasher);
            (outcome.status.as_str(), outcome.exit_code).hash(&mut hasher);
        }
        content @ (ConversationItem::FileContext { .. }
        | ConversationItem::Compaction { .. }
        | ConversationItem::System { .. }
        | ConversationItem::User { .. }) => content.display_text().hash(&mut hasher),
    }
    hasher.finish()
}

#[cfg(test)]
mod tests;
