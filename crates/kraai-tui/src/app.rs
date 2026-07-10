use std::collections::HashMap;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use color_eyre::eyre::Result;
use crossbeam_channel::{Receiver, Sender};
use kraai_runtime::{
    AgentProfilesState, Event, FieldDefinition, Model, ModelSettings, ProviderDefinition,
    ProviderSettings, RuntimeHandle, SettingsValue, ToolBatchOutcome,
};
use kraai_types::{ChatRole, MessageId, MessageStatus};
use ratatui::{
    crossterm::event::{
        self, Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
        MouseEventKind,
    },
    layout::{Constraint, Flex, Layout},
};

use crate::components::TextInput;

mod auth;
mod chat_tools;
mod lifecycle;
mod providers_flow;
mod runtime_bridge;
mod runtime_handlers;
mod session_commands;
mod settings;
mod settings_flow;
mod state;
mod terminal;
mod types;
mod ui;
mod workspace_preferences;
use self::auth::{
    ProviderAuthState, ProviderAuthStatus, map_openai_codex_auth_status, open_external_target,
    pending_auth_target,
};
use self::runtime_bridge::{RuntimeEventBridgeMessage, spawn_event_bridge, spawn_runtime_bridge};
use self::settings::{
    clear_field_value, default_values, field_value_display, flatten_models_map, is_boolean_field,
    merge_values, next_provider_id, parse_field_input, provider_definition_rank, set_field_value,
};
use self::state::{AppState, build_tip_chain};
pub use self::types::StartupOptions;
use self::types::default_agent_profiles;
use self::types::{
    ActiveSettingsEditor, DEFAULT_AGENT_PROFILE_ID, OptimisticMessage, OptimisticToolMessage,
    PendingSubmit, PendingTool, ProviderDetailAction, ProvidersAdvancedFocus, ProvidersView,
    RuntimeRequest, RuntimeResponse, SettingsFocus, SettingsModelField, SettingsProviderField,
    ToolApprovalAction, ToolPhase, UiMode, UsageModelKey,
};
use self::ui::{
    STATUSLINE_STREAMING_FRAMES, active_command_prefix, adjust_index, bottom_panel_height,
    copy_via_osc52, format_token_count, is_known_slash_command, model_menu_next_index,
    model_menu_previous_index, parse_settings_errors, slash_command_matches,
};
use self::workspace_preferences::WorkspacePreferences;

const SLASH_COMMANDS: [(&str, &str); 9] = [
    ("agent", "Open agent selector"),
    ("continue", "Reprompt the agent"),
    ("help", "Open command help"),
    ("model", "Open model selector"),
    ("new", "Start new chat"),
    ("providers", "Open providers"),
    ("quit", "Exit Kraai"),
    ("sessions", "Open sessions menu"),
    ("undo", "Restore last user message"),
];

pub struct App {
    event_rx: Receiver<RuntimeEventBridgeMessage>,
    runtime_tx: Sender<RuntimeRequest>,
    runtime_rx: Receiver<RuntimeResponse>,
    clipboard: Option<arboard::Clipboard>,
    ci_output: Box<dyn Write + Send>,
    ci_output_needs_newline: bool,
    ci_turn_completion_pending: bool,
    startup_options: StartupOptions,
    startup_message_sent: bool,
    ci_error: Option<String>,
    stream_event_content: HashMap<MessageId, String>,
    state: AppState,
    last_stream_history_request: Option<Instant>,
    last_statusline_animation_tick: Option<Instant>,
    event_lag_session_resync_pending: bool,
    event_lag_tools_resync_pending: bool,
}

const STATUSLINE_ANIMATION_INTERVAL: Duration = Duration::from_millis(120);
const STREAM_HISTORY_SYNC_FALLBACK_INTERVAL: Duration = Duration::from_millis(50);
const INPUT_HISTORY_LIMIT: usize = 100;

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "TUI tests use direct assertions for buffered output and request ordering"
)]
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
            model_usage.cache_read_tokens = model_usage
                .cache_read_tokens
                .saturating_add(usage.cache_read_tokens);
        }
    }

    pub(super) fn append_stream_chunk_to_cached_message(
        &mut self,
        message_id: &str,
        chunk: &str,
    ) -> bool {
        let message_id = MessageId::new(message_id);
        if self
            .state
            .chat_history
            .get(&message_id)
            .is_some_and(|message| !matches!(message.status, MessageStatus::Streaming { .. }))
        {
            return true;
        }

        let event_content = self
            .stream_event_content
            .entry(message_id.clone())
            .or_default();
        event_content.push_str(chunk);

        let Some(message) = self.state.chat_history.get_mut(&message_id) else {
            return false;
        };
        let changed =
            merge_stream_chunk_into_cached_content(&mut message.content, event_content, chunk);
        if changed {
            self.invalidate_chat_cache();
            self.clamp_chat_scroll();
        }
        true
    }

    pub(super) fn request_stream_history_sync(&mut self, session_id: &str, now: Instant) {
        let should_request = self
            .last_stream_history_request
            .is_none_or(|last| now.duration_since(last) >= STREAM_HISTORY_SYNC_FALLBACK_INTERVAL);
        if !should_request {
            return;
        }

        self.last_stream_history_request = Some(now);
        self.request(RuntimeRequest::GetCurrentTip {
            session_id: session_id.to_string(),
        });
        self.request(RuntimeRequest::GetChatHistory {
            session_id: session_id.to_string(),
        });
    }

    pub(super) fn merge_local_streaming_content(
        &mut self,
        history: &mut std::collections::BTreeMap<MessageId, kraai_types::Message>,
    ) {
        for (message_id, incoming) in history {
            if !matches!(incoming.status, MessageStatus::Streaming { .. }) {
                self.stream_event_content.remove(message_id);
                continue;
            }

            if let Some(current) = self.state.chat_history.get(message_id)
                && matches!(current.status, MessageStatus::Streaming { .. })
            {
                merge_newer_streaming_prefix(&mut incoming.content, &current.content);
            }

            if let Some(event_content) = self.stream_event_content.get(message_id) {
                merge_newer_streaming_prefix(&mut incoming.content, event_content);
            }
        }
    }
}

fn merge_newer_streaming_prefix(incoming_content: &mut String, candidate_content: &str) {
    if candidate_content.len() > incoming_content.len()
        && candidate_content.starts_with(incoming_content.as_str())
    {
        incoming_content.clear();
        incoming_content.push_str(candidate_content);
    }
}

fn merge_stream_chunk_into_cached_content(
    cached_content: &mut String,
    event_content: &mut String,
    chunk: &str,
) -> bool {
    if cached_content.starts_with(event_content.as_str()) {
        return false;
    }

    if event_content.starts_with(cached_content.as_str()) {
        cached_content.clone_from(event_content);
        return true;
    }

    if cached_content.ends_with(chunk) {
        event_content.clone_from(cached_content);
        return false;
    }

    cached_content.push_str(chunk);
    event_content.clone_from(cached_content);
    true
}
