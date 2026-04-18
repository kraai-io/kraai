use std::collections::HashMap;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use color_eyre::eyre::Result;
use crossbeam_channel::{Receiver, Sender};
use kraai_runtime::{
    AgentProfilesState, Event, FieldDefinition, Model, ModelSettings, ProviderDefinition,
    ProviderSettings, RuntimeHandle, SettingsValue,
};
use kraai_types::ChatRole;
use ratatui::{
    crossterm::event::{
        self, Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
        MouseEvent, MouseEventKind,
    },
    layout::{Constraint, Flex, Layout},
};

use crate::components::{ChatHistory, TextInput};

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
use self::auth::{
    ProviderAuthState, ProviderAuthStatus, map_openai_codex_auth_status, open_external_target,
    pending_auth_target,
};
use self::runtime_bridge::{spawn_event_bridge, spawn_runtime_bridge};
use self::settings::{
    clear_field_value, default_values, field_value_display, flatten_models_map, is_boolean_field,
    merge_values, next_provider_id, parse_field_input, provider_definition_rank, set_field_value,
};
use self::state::{AppState, build_tip_chain};
pub use self::types::StartupOptions;
use self::types::default_agent_profiles;
use self::types::{
    ActiveSettingsEditor, ChatCellPosition, ChatSelection, DEFAULT_AGENT_PROFILE_ID,
    OptimisticMessage, OptimisticToolMessage, PendingSubmit, PendingTool, ProviderDetailAction,
    ProvidersAdvancedFocus, ProvidersView, RuntimeRequest, RuntimeResponse, SettingsFocus,
    SettingsModelField, SettingsProviderField, ToolApprovalAction, ToolPhase, UiMode,
};
use self::ui::{
    STATUSLINE_STREAMING_FRAMES, active_command_prefix, adjust_index, bottom_panel_height,
    copy_via_osc52, is_copy_shortcut, is_known_slash_command, model_menu_next_index,
    model_menu_previous_index, parse_settings_errors, selection_text, slash_command_matches,
};
#[cfg(test)]
use self::ui::{menu_scroll_offset, render_chat_selection_overlay};

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
    event_rx: Receiver<Event>,
    runtime_tx: Sender<RuntimeRequest>,
    runtime_rx: Receiver<RuntimeResponse>,
    clipboard: Option<arboard::Clipboard>,
    ci_output: Box<dyn Write + Send>,
    ci_output_needs_newline: bool,
    ci_turn_completion_pending: bool,
    startup_options: StartupOptions,
    startup_message_sent: bool,
    ci_error: Option<String>,
    state: AppState,
    last_stream_refresh: Option<Instant>,
    last_statusline_animation_tick: Option<Instant>,
}

const STATUSLINE_ANIMATION_INTERVAL: Duration = Duration::from_millis(120);

#[cfg(test)]
mod tests;
