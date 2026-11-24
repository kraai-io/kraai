use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use kraai_types::{AssistantItem, ConversationItem, MessageId, MessageStatus, SessionId};

mod agent_sheet;
mod auth;
mod chat;
mod chat_render;
mod clipboard_worker;
mod commands;
mod composer;
mod cost;
mod cursor;
mod cursor_preferences;
mod draft_images;
mod error;
mod executions;
mod feedback;
mod frame_pacer;
mod images;
mod input;
mod lifecycle;
mod line_edit;
mod mcp_auth;
mod model_options;
mod model_sheet;
mod palette;
mod provider_config;
mod providers_sheet;
mod refreshes;
mod runtime_bridge;
mod runtime_events;
mod runtime_responses;
mod session_commands;
mod session_model;
mod sessions_sheet;
mod settings;
mod sheet_query;
mod state;
mod terminal;
mod terminal_reply;
mod types;
mod ui;
mod workspace_preferences;
use self::runtime_bridge::RuntimeEventBridgeMessage;
use self::state::AppState;
pub use self::types::StartupOptions;
use self::types::{RuntimeRequest, RuntimeResponse};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeRequestDelivery {
    Delivered,
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupSync {
    WaitingForRuntime,
    Synchronizing,
    Complete,
}

pub struct App {
    event_rx: Receiver<RuntimeEventBridgeMessage>,
    runtime_tx: Sender<RuntimeRequest>,
    runtime_rx: Receiver<RuntimeResponse>,
    input: input::TerminalInput,
    hangup: Receiver<()>,
    clipboard: Option<arboard::Clipboard>,
    startup_options: StartupOptions,
    persist_workspace_preferences: bool,
    sandbox_check: fn() -> Option<String>,
    startup_message_sent: bool,
    startup_sync: StartupSync,
    state: AppState,
    last_statusline_animation_tick: Option<Instant>,
    last_runtime_event_sequence: u64,
    refreshes: refreshes::Refreshes,
    session_snapshot_sequences: HashMap<SessionId, u64>,
    event_lag_session_resync_pending: bool,
    event_lag_script_resync_pending: bool,
    runtime_bridge_connected: bool,
    runtime_bridge_error: Option<String>,
    runtime_event_batch_budget: Duration,
}

const STATUSLINE_ANIMATION_INTERVAL: Duration = Duration::from_millis(120);
const INPUT_HISTORY_LIMIT: usize = 100;

#[cfg(test)]
mod tests;

impl App {
    pub(super) fn discard_streamed_content(&mut self, message_id: &str) {
        if let Some(message) = self.state.chat.history.get_mut(&MessageId::new(message_id))
            && matches!(message.status, MessageStatus::Streaming { .. })
        {
            message.content = ConversationItem::Assistant { items: Vec::new() };
            self.state.chat.invalidate();
            self.clamp_chat_scroll();
        }
    }

    pub(super) fn apply_item_started(
        &mut self,
        message_id: &str,
        index: usize,
        item: AssistantItem,
    ) -> bool {
        let Some(items) = self.streaming_items(message_id) else {
            return false;
        };
        match index.cmp(&items.len()) {
            std::cmp::Ordering::Less => {
                if let Some(existing) = items.get_mut(index) {
                    *existing = item;
                }
            }
            std::cmp::Ordering::Equal => items.push(item),
            std::cmp::Ordering::Greater => return false,
        }
        self.mark_streamed(message_id);
        self.clamp_chat_scroll();
        true
    }

    pub(super) fn apply_item_delta(&mut self, message_id: &str, index: usize, delta: &str) -> bool {
        let Some(items) = self.streaming_items(message_id) else {
            return false;
        };
        let Some(AssistantItem::Text { text, .. }) = items.get_mut(index) else {
            return false;
        };
        text.push_str(delta);
        self.mark_streamed(message_id);
        self.clamp_chat_scroll();
        true
    }

    fn mark_streamed(&mut self, message_id: &str) {
        self.state
            .chat
            .render_cache
            .get_mut()
            .mark_streamed(MessageId::new(message_id));
    }

    fn streaming_items(&mut self, message_id: &str) -> Option<&mut Vec<AssistantItem>> {
        let message = self
            .state
            .chat
            .history
            .get_mut(&MessageId::new(message_id))?;
        if !matches!(message.status, MessageStatus::Streaming { .. }) {
            return None;
        }
        let ConversationItem::Assistant { items } = &mut message.content else {
            return None;
        };
        Some(items)
    }

    pub(super) fn install_latest_history(
        &mut self,
        session_id: &SessionId,
        latest: std::collections::BTreeMap<MessageId, kraai_types::Message>,
        older: Option<MessageId>,
    ) {
        match older {
            None => self.state.chat.history = latest,
            Some(older) => {
                if let Some(oldest) = latest.keys().next() {
                    self.state.chat.history.retain(|id, _| id < oldest);
                }
                self.state.chat.history.extend(latest);
                if !self.state.chat.history.contains_key(&older) {
                    self.request(RuntimeRequest::GetHistoryPage {
                        session_id: session_id.clone(),
                        from: older,
                    });
                }
            }
        }
    }

    pub(super) fn install_older_history(
        &mut self,
        session_id: &SessionId,
        page: kraai_runtime::HistoryPage,
    ) {
        self.state.usage.accumulate_completed(&page.messages);
        for (id, message) in page.messages {
            self.state.chat.history.entry(id).or_insert(message);
        }
        if let Some(older) = page.older_history
            && !self.state.chat.history.contains_key(&older)
        {
            self.request(RuntimeRequest::GetHistoryPage {
                session_id: session_id.clone(),
                from: older,
            });
        }
        self.state.chat.invalidate();
        self.reconcile_optimistic_messages();
        self.clamp_chat_scroll();
    }

    pub(super) fn upsert_message(&mut self, message: kraai_types::Message) {
        let completed = message.status == MessageStatus::Complete;
        let mut history = std::collections::BTreeMap::new();
        history.insert(message.id.clone(), message);
        if completed {
            self.state.usage.accumulate_completed(&history);
        }
        self.state.chat.history.extend(history);
        self.state.chat.invalidate();
        self.reconcile_optimistic_messages();
        self.clamp_chat_scroll();
    }

    pub(super) fn remove_messages(&mut self, message_ids: &[MessageId]) {
        for id in message_ids {
            self.state.chat.history.remove(id);
        }
        self.state.chat.invalidate();
        self.clamp_chat_scroll();
    }
}
