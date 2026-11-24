use std::cell::RefCell;
use std::collections::BTreeMap;

use kraai_types::{Message, MessageId};

use crate::app::chat_render::ChatRenderCache;
use crate::app::types::OptimisticMessage;

pub(in crate::app) struct ChatView {
    pub(in crate::app) history: BTreeMap<MessageId, Message>,
    pub(in crate::app) tip_id: Option<String>,
    pub(in crate::app) optimistic_messages: Vec<OptimisticMessage>,
    pub(in crate::app) optimistic_seq: u64,
    pub(in crate::app) epoch: u64,
    pub(in crate::app) cards_epoch: u64,
    pub(in crate::app) render_cache: RefCell<ChatRenderCache>,
    pub(in crate::app) viewport_height: u16,
    pub(in crate::app) scroll: usize,
    pub(in crate::app) auto_scroll: bool,
    pub(in crate::app) scripts_expanded: bool,
}

impl Default for ChatView {
    fn default() -> Self {
        Self {
            history: BTreeMap::new(),
            tip_id: None,
            optimistic_messages: Vec::new(),
            optimistic_seq: 0,
            epoch: 0,
            cards_epoch: 0,
            render_cache: RefCell::new(ChatRenderCache::default()),
            viewport_height: 0,
            scroll: 0,
            auto_scroll: true,
            scripts_expanded: false,
        }
    }
}

impl ChatView {
    pub(in crate::app) fn max_scroll(&self) -> usize {
        self.render_cache
            .borrow()
            .total_lines
            .saturating_sub(usize::from(self.viewport_height))
    }

    pub(in crate::app) fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }

    pub(in crate::app) fn invalidate_cards(&mut self) {
        self.cards_epoch = self.cards_epoch.wrapping_add(1);
    }
}
