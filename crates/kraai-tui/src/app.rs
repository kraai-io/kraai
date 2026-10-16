use std::collections::HashMap;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use color_eyre::eyre::Result;
use crossbeam_channel::{Receiver, Sender};
use kraai_execution_types::ImageAttachment;
use kraai_runtime::{
    AgentProfilesState, Event, FieldDefinition, Model, ModelSettings, ProviderDefinition,
    ProviderSettings, RuntimeHandle, SettingsValue,
};
use kraai_types::{
    AssistantItem, ChatRole, ConversationItem, MessageContent, MessageId, MessageStatus, SessionId,
};
use ratatui::crossterm::event::{
    Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
    MouseEventKind,
};

use crate::components::TextInput;

mod auth;
mod chat;
mod chat_render;
mod clipboard_worker;
mod composer;
mod cost;
mod cursor;
mod cursor_preferences;
mod draft_images;
mod duration;
mod error;
mod executions;
mod feedback;
mod images;
mod input;
mod lifecycle;
mod mcp_auth;
mod menu_search;
mod model_options;
mod palette;
mod providers_flow;
mod runtime_bridge;
mod runtime_events;
mod runtime_responses;
mod session_commands;
mod session_model;
mod settings;
mod settings_flow;
mod state;
mod terminal;
mod terminal_reply;
mod types;
mod ui;
mod workspace_preferences;
use self::auth::{
    ProviderAuthState, ProviderAuthStatus, map_provider_auth_status, open_external_target,
    pending_auth_target,
};
use self::chat_render::build_tip_chain;
use self::runtime_bridge::{RuntimeEventBridgeMessage, spawn_event_bridge, spawn_runtime_bridge};
use self::settings::{
    clear_field_value, default_values, field_value_display, is_boolean_field, merge_values,
    next_provider_id, parse_field_input, provider_definition_rank, set_field_value,
};
use self::state::AppState;
pub use self::types::StartupOptions;
use self::types::{
    ActiveSettingsEditor, OptimisticMessage, PendingSubmit, ProviderDetailAction,
    ProvidersAdvancedFocus, ProvidersView, RuntimeRequest, RuntimeResponse, ScriptApprovalAction,
    ScriptPhase, SettingsFocus, SettingsModelField, SettingsProviderField, UiMode, UsageModelKey,
};
use self::ui::{
    STATUSLINE_STREAMING_FRAMES, active_command_prefix, adjust_index, copy_via_osc52,
    format_token_count, is_known_slash_command, model_menu_next_index, model_menu_previous_index,
    slash_command_matches,
};
use self::workspace_preferences::WorkspacePreferences;

const SLASH_COMMANDS: [(&str, &str); 11] = [
    ("agent", "Open agent selector"),
    ("continue", "Reprompt the agent"),
    ("help", "Open command help"),
    ("model", "Open model selector"),
    ("option", "Configure selected model options"),
    ("new", "Start new chat"),
    ("providers", "Open providers"),
    ("mcp", "MCP servers and sign-in"),
    ("quit", "Exit Kraai"),
    ("sessions", "Open sessions menu"),
    ("undo", "Restore last user message"),
];

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
    /// Whether model choices are remembered for the workspace; off in tests.
    persist_workspace_preferences: bool,
    startup_message_sent: bool,
    startup_sync: StartupSync,
    state: AppState,
    last_statusline_animation_tick: Option<Instant>,
    last_runtime_event_sequence: u64,
    last_session_event_sequences: HashMap<SessionId, u64>,
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
    pub(super) fn mark_exit_usage_message_completed(&mut self, message_id: kraai_types::MessageId) {
        self.state
            .exit_usage_totals
            .completed_message_ids
            .insert(message_id);
    }

    pub(super) fn accumulate_exit_usage_from_history(
        &mut self,
        history: &std::collections::BTreeMap<kraai_types::MessageId, kraai_types::Message>,
    ) {
        for message in history.values() {
            if !self
                .state
                .exit_usage_totals
                .completed_message_ids
                .contains(&message.id)
            {
                continue;
            }
            let Some(generation) = message.generation.as_ref() else {
                continue;
            };
            let Some(usage) = generation.usage.as_ref() else {
                continue;
            };
            if !self
                .state
                .exit_usage_totals
                .counted_message_ids
                .insert(message.id.clone())
            {
                continue;
            }

            let model_usage = self
                .state
                .exit_usage_totals
                .usage_by_model
                .entry(UsageModelKey {
                    provider_id: generation.provider_id.to_string(),
                    model_id: generation.model_id.to_string(),
                })
                .or_default();
            model_usage.total_tokens = model_usage.total_tokens.saturating_add(usage.total_tokens);
            model_usage.input_tokens = model_usage.input_tokens.saturating_add(usage.input_tokens);
            model_usage.output_tokens = model_usage
                .output_tokens
                .saturating_add(usage.output_tokens);
            model_usage.reasoning_tokens = model_usage
                .reasoning_tokens
                .saturating_add(usage.reasoning_tokens);
            model_usage.cache_write_tokens = model_usage
                .cache_write_tokens
                .saturating_add(usage.cache_write_tokens);
            model_usage.cache_read_tokens = model_usage
                .cache_read_tokens
                .saturating_add(usage.cache_read_tokens);
        }
    }

    pub(super) fn discard_streamed_content(&mut self, message_id: &str) {
        if let Some(message) = self.state.chat_history.get_mut(&MessageId::new(message_id))
            && matches!(message.status, MessageStatus::Streaming { .. })
        {
            message.content = ConversationItem::Assistant { items: Vec::new() };
            self.invalidate_chat_cache();
            self.clamp_chat_scroll();
        }
    }

    /// Adds a streamed item to the cached message; false when the cache cannot take it and
    /// needs a snapshot.
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
        self.invalidate_chat_cache();
        self.clamp_chat_scroll();
        true
    }

    /// Appends streamed text to a cached item; false when the cache cannot take it.
    pub(super) fn apply_item_delta(&mut self, message_id: &str, index: usize, delta: &str) -> bool {
        let Some(items) = self.streaming_items(message_id) else {
            return false;
        };
        let Some(AssistantItem::Text { text, .. }) = items.get_mut(index) else {
            return false;
        };
        text.push_str(delta);
        self.invalidate_chat_cache();
        self.clamp_chat_scroll();
        true
    }

    fn streaming_items(&mut self, message_id: &str) -> Option<&mut Vec<AssistantItem>> {
        let message = self
            .state
            .chat_history
            .get_mut(&MessageId::new(message_id))?;
        if !matches!(message.status, MessageStatus::Streaming { .. }) {
            return None;
        }
        match &mut message.content {
            ConversationItem::Assistant { items } => Some(items),
            _ => None,
        }
    }

    /// Installs the latest history from a snapshot, keeping older messages already loaded.
    pub(super) fn install_latest_history(
        &mut self,
        session_id: &SessionId,
        latest: std::collections::BTreeMap<MessageId, kraai_types::Message>,
        older: Option<MessageId>,
    ) {
        match older {
            None => self.state.chat_history = latest,
            Some(older) => {
                if let Some(oldest) = latest.keys().next() {
                    self.state.chat_history.retain(|id, _| id < oldest);
                }
                self.state.chat_history.extend(latest);
                if !self.state.chat_history.contains_key(&older) {
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
        self.accumulate_exit_usage_from_history(&page.messages);
        for (id, message) in page.messages {
            self.state.chat_history.entry(id).or_insert(message);
        }
        if let Some(older) = page.older_history
            && !self.state.chat_history.contains_key(&older)
        {
            self.request(RuntimeRequest::GetHistoryPage {
                session_id: session_id.clone(),
                from: older,
            });
        }
        self.invalidate_chat_cache();
        self.reconcile_optimistic_messages();
        self.clamp_chat_scroll();
    }

    pub(super) fn upsert_message(&mut self, message: kraai_types::Message) {
        let completed = message.status == MessageStatus::Complete;
        let mut history = std::collections::BTreeMap::new();
        history.insert(message.id.clone(), message);
        if completed {
            self.accumulate_exit_usage_from_history(&history);
        }
        self.state.chat_history.extend(history);
        self.invalidate_chat_cache();
        self.reconcile_optimistic_messages();
        self.clamp_chat_scroll();
    }

    pub(super) fn remove_messages(&mut self, message_ids: &[MessageId]) {
        for id in message_ids {
            self.state.chat_history.remove(id);
        }
        self.invalidate_chat_cache();
        self.clamp_chat_scroll();
    }
}
