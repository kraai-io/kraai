use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_runtime::{
    AgentProfileSummary, AgentProfileWarning, AgentProfilesState, Event, FieldDefinition,
    FieldValueEntry, Model, ModelSettings, OpenAiCodexAuthStatus as RuntimeOpenAiCodexAuthStatus,
    OpenAiCodexLoginState as RuntimeOpenAiCodexLoginState, ProviderDefinition, ProviderSettings,
    RuntimeHandle, Session, SettingsDocument, SettingsValue,
};
use color_eyre::eyre::Result;
use crossbeam_channel::{Receiver, Sender};
use ratatui::{
    crossterm::event::{
        self, Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
        MouseEvent, MouseEventKind,
    },
    layout::{Constraint, Flex, Layout},
};
use types::{ChatRole, Message, MessageId, MessageStatus, RiskLevel};

use crate::components::{ChatHistory, RenderedLine, TextInput, VisibleChatView};

mod runtime_bridge;
mod ui;

use self::runtime_bridge::spawn_runtime_bridge;
use self::ui::{
    active_command_prefix, adjust_index, bottom_panel_height, copy_via_osc52, is_copy_shortcut,
    is_known_slash_command, model_menu_next_index, model_menu_previous_index,
    parse_settings_errors, selection_text, slash_command_matches,
};
#[cfg(test)]
use self::ui::{menu_scroll_offset, render_chat_selection_overlay};

const SLASH_COMMANDS: [(&str, &str); 7] = [
    ("agent", "Open agent selector"),
    ("help", "Open command help"),
    ("model", "Open model selector"),
    ("new", "Start new chat"),
    ("providers", "Open providers"),
    ("quit", "Exit the TUI"),
    ("sessions", "Open sessions menu"),
];

fn default_agent_profiles() -> Vec<AgentProfileSummary> {
    vec![
        AgentProfileSummary {
            id: String::from("plan-code"),
            display_name: String::from("Plan Code"),
            description: String::from("Read-only planning agent"),
            tools: vec![
                String::from("list_files"),
                String::from("search_files"),
                String::from("read_files"),
            ],
            default_risk_level: RiskLevel::ReadOnlyWorkspace,
            source: agent_runtime::AgentProfileSource::BuiltIn,
        },
        AgentProfileSummary {
            id: String::from("build-code"),
            display_name: String::from("Build Code"),
            description: String::from("Implementation agent"),
            tools: vec![
                String::from("list_files"),
                String::from("search_files"),
                String::from("read_files"),
                String::from("edit_file"),
            ],
            default_risk_level: RiskLevel::UndoableWorkspaceWrite,
            source: agent_runtime::AgentProfileSource::BuiltIn,
        },
    ]
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum UiMode {
    Chat,
    AgentMenu,
    ModelMenu,
    ProvidersMenu,
    SessionsMenu,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProvidersView {
    List,
    Connect,
    Detail,
    Advanced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolApprovalAction {
    Allow,
    Reject,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolPhase {
    Idle,
    Deciding,
    ExecutingBatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsFocus {
    ProviderList,
    ProviderForm,
    ModelList,
    ModelForm,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SettingsProviderField {
    Id,
    TypeId,
    Value(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SettingsModelField {
    Id,
    Value(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ActiveSettingsEditor {
    Provider(SettingsProviderField),
    Model(SettingsModelField),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderDetailAction {
    BrowserLogin,
    DeviceCodeLogin,
    CancelLogin,
    Logout,
    Advanced,
    RefreshModels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProvidersAdvancedFocus {
    ProviderFields,
    Models,
    ModelFields,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum ProviderAuthState {
    #[default]
    SignedOut,
    BrowserPending,
    DeviceCodePending,
    Authenticated,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ProviderAuthStatus {
    state: ProviderAuthState,
    email: Option<String>,
    plan_type: Option<String>,
    account_id: Option<String>,
    last_refresh: Option<String>,
    auth_url: Option<String>,
    verification_url: Option<String>,
    user_code: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
struct PendingTool {
    call_id: String,
    tool_id: String,
    args: String,
    description: String,
    risk_level: String,
    reasons: Vec<String>,
    approved: Option<bool>,
    queue_order: u64,
}

#[derive(Clone, Debug)]
struct OptimisticMessage {
    local_id: String,
    content: String,
    content_key: String,
    occurrence: usize,
    is_queued: bool,
}

#[derive(Clone, Debug)]
struct OptimisticToolMessage {
    local_id: String,
    content: String,
}

#[derive(Clone, Debug)]
struct PendingSubmit {
    session_id: Option<String>,
    message: String,
    model_id: String,
    provider_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChatCellPosition {
    line: usize,
    column: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChatSelection {
    anchor: ChatCellPosition,
    focus: ChatCellPosition,
}

impl ChatSelection {
    fn normalized(self) -> (ChatCellPosition, ChatCellPosition) {
        if self.anchor.line < self.focus.line
            || (self.anchor.line == self.focus.line && self.anchor.column <= self.focus.column)
        {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }
}

enum RuntimeRequest {
    ListModels,
    ListAgentProfiles {
        session_id: String,
    },
    ListProviderDefinitions,
    GetSettings,
    GetOpenAiCodexAuthStatus,
    StartOpenAiCodexBrowserLogin,
    StartOpenAiCodexDeviceCodeLogin,
    CancelOpenAiCodexLogin,
    LogoutOpenAiCodexAuth,
    CreateSession,
    SetSessionProfile {
        session_id: String,
        profile_id: String,
    },
    SendMessage {
        session_id: String,
        message: String,
        model_id: String,
        provider_id: String,
    },
    SaveSettings {
        settings: SettingsDocument,
    },
    GetChatHistory {
        session_id: String,
    },
    GetCurrentTip {
        session_id: String,
    },
    GetPendingTools {
        session_id: String,
    },
    LoadSession {
        session_id: String,
    },
    ListSessions,
    DeleteSession {
        session_id: String,
    },
    ApproveTool {
        session_id: String,
        call_id: String,
    },
    DenyTool {
        session_id: String,
        call_id: String,
    },
    CancelStream {
        session_id: String,
    },
    ExecuteApprovedTools {
        session_id: String,
    },
}

enum RuntimeResponse {
    Models(Result<HashMap<String, Vec<Model>>, String>),
    AgentProfiles {
        session_id: String,
        result: Result<AgentProfilesState, String>,
    },
    ProviderDefinitions(Result<Vec<ProviderDefinition>, String>),
    Settings(Result<SettingsDocument, String>),
    OpenAiCodexAuthStatus(Result<ProviderAuthStatus, String>),
    StartOpenAiCodexBrowserLogin(Result<ProviderAuthStatus, String>),
    StartOpenAiCodexDeviceCodeLogin(Result<ProviderAuthStatus, String>),
    CancelOpenAiCodexLogin(Result<ProviderAuthStatus, String>),
    LogoutOpenAiCodexAuth(Result<ProviderAuthStatus, String>),
    CreateSession(Result<String, String>),
    SetSessionProfile {
        profile_id: String,
        result: Result<(), String>,
    },
    SendMessage(Result<(), String>),
    SaveSettings(Result<(), String>),
    ChatHistory {
        session_id: String,
        result: Result<BTreeMap<MessageId, Message>, String>,
    },
    CurrentTip {
        session_id: String,
        result: Result<Option<String>, String>,
    },
    PendingTools {
        session_id: String,
        result: Result<Vec<agent_runtime::PendingToolInfo>, String>,
    },
    LoadSession {
        session_id: String,
        result: Result<bool, String>,
    },
    Sessions(Result<Vec<Session>, String>),
    DeleteSession {
        session_id: String,
        result: Result<(), String>,
    },
    ApproveTool {
        call_id: String,
        result: Result<(), String>,
    },
    DenyTool {
        call_id: String,
        result: Result<(), String>,
    },
    CancelStream(Result<bool, String>),
    ExecuteApprovedTools(Result<(), String>),
}

pub struct App {
    event_rx: Receiver<Event>,
    runtime_tx: Sender<RuntimeRequest>,
    runtime_rx: Receiver<RuntimeResponse>,
    clipboard: Option<arboard::Clipboard>,
    state: AppState,
    last_stream_refresh: Option<Instant>,
}

const DEFAULT_AGENT_PROFILE_ID: &str = "plan-code";

pub struct AppState {
    exit: bool,
    input: String,
    input_cursor: usize,
    ctrl_c_exit_armed: bool,
    chat_history: BTreeMap<MessageId, Message>,
    optimistic_messages: Vec<OptimisticMessage>,
    optimistic_tool_messages: Vec<OptimisticToolMessage>,
    optimistic_seq: u64,
    chat_epoch: u64,
    chat_render_cache: RefCell<ChatRenderCache>,
    chat_viewport_height: u16,
    scroll: u16,
    auto_scroll: bool,
    selection: Option<ChatSelection>,
    visible_chat_view: Option<VisibleChatView>,
    config_loaded: bool,
    mode: UiMode,
    status: String,
    is_streaming: bool,
    models_by_provider: HashMap<String, Vec<Model>>,
    agent_profiles: Vec<AgentProfileSummary>,
    agent_profile_warnings: Vec<AgentProfileWarning>,
    provider_definitions: Vec<ProviderDefinition>,
    selected_profile_id: Option<String>,
    profile_locked: bool,
    selected_provider_id: Option<String>,
    selected_model_id: Option<String>,
    pending_tools: Vec<PendingTool>,
    sessions: Vec<Session>,
    current_session_id: Option<String>,
    current_tip_id: Option<String>,
    agent_menu_index: usize,
    model_menu_index: usize,
    sessions_menu_index: usize,
    tool_approval_action: ToolApprovalAction,
    tool_phase: ToolPhase,
    tool_batch_execution_started: bool,
    command_completion_prefix: Option<String>,
    command_completion_index: usize,
    command_popup_dismissed: bool,
    settings_draft: Option<SettingsDocument>,
    settings_errors: HashMap<String, String>,
    settings_focus: SettingsFocus,
    settings_provider_index: usize,
    settings_model_index: usize,
    settings_provider_field_index: usize,
    settings_model_field_index: usize,
    settings_editor: Option<ActiveSettingsEditor>,
    settings_editor_input: String,
    settings_delete_armed: bool,
    providers_view: ProvidersView,
    providers_advanced_focus: ProvidersAdvancedFocus,
    connect_provider_search: String,
    connect_provider_index: usize,
    openai_codex_auth: ProviderAuthStatus,
    pending_submit: Option<PendingSubmit>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            exit: false,
            input: String::new(),
            input_cursor: 0,
            ctrl_c_exit_armed: false,
            chat_history: BTreeMap::new(),
            optimistic_messages: Vec::new(),
            optimistic_tool_messages: Vec::new(),
            optimistic_seq: 0,
            chat_epoch: 0,
            chat_render_cache: RefCell::new(ChatRenderCache::default()),
            chat_viewport_height: 0,
            scroll: 0,
            auto_scroll: true,
            selection: None,
            visible_chat_view: None,
            config_loaded: false,
            mode: UiMode::Chat,
            status: String::from("Type /help for commands"),
            is_streaming: false,
            models_by_provider: HashMap::new(),
            agent_profiles: default_agent_profiles(),
            agent_profile_warnings: Vec::new(),
            provider_definitions: Vec::new(),
            selected_profile_id: Some(String::from(DEFAULT_AGENT_PROFILE_ID)),
            profile_locked: false,
            selected_provider_id: None,
            selected_model_id: None,
            pending_tools: Vec::new(),
            sessions: Vec::new(),
            current_session_id: None,
            current_tip_id: None,
            agent_menu_index: 0,
            model_menu_index: 0,
            sessions_menu_index: 0,
            tool_approval_action: ToolApprovalAction::Allow,
            tool_phase: ToolPhase::Idle,
            tool_batch_execution_started: false,
            command_completion_prefix: None,
            command_completion_index: 0,
            command_popup_dismissed: false,
            settings_draft: None,
            settings_errors: HashMap::new(),
            settings_focus: SettingsFocus::ProviderList,
            settings_provider_index: 0,
            settings_model_index: 0,
            settings_provider_field_index: 0,
            settings_model_field_index: 0,
            settings_editor: None,
            settings_editor_input: String::new(),
            settings_delete_armed: false,
            providers_view: ProvidersView::List,
            providers_advanced_focus: ProvidersAdvancedFocus::ProviderFields,
            connect_provider_search: String::new(),
            connect_provider_index: 0,
            openai_codex_auth: ProviderAuthStatus::default(),
            pending_submit: None,
        }
    }
}

#[derive(Default)]
struct ChatRenderCache {
    width: u16,
    epoch: u64,
    sections: Vec<Arc<Vec<RenderedLine>>>,
    total_lines: u16,
    message_cache: HashMap<String, CachedMessageRender>,
}

struct CachedMessageRender {
    fingerprint: u64,
    lines: Arc<Vec<RenderedLine>>,
}

fn default_values(fields: &[FieldDefinition]) -> Vec<FieldValueEntry> {
    fields
        .iter()
        .filter_map(|field| {
            field.default_value.clone().map(|value| FieldValueEntry {
                key: field.key.clone(),
                value,
            })
        })
        .collect()
}

fn merge_values(fields: &[FieldDefinition], existing: &[FieldValueEntry]) -> Vec<FieldValueEntry> {
    fields
        .iter()
        .filter_map(|field| {
            existing
                .iter()
                .find(|value| value.key == field.key)
                .cloned()
                .or_else(|| {
                    field.default_value.clone().map(|value| FieldValueEntry {
                        key: field.key.clone(),
                        value,
                    })
                })
        })
        .collect()
}

pub(super) fn field_value_display(values: &[FieldValueEntry], key: &str) -> String {
    values
        .iter()
        .find(|value| value.key == key)
        .map(|value| match &value.value {
            SettingsValue::String(value) => value.clone(),
            SettingsValue::Bool(value) => {
                if *value {
                    String::from("yes")
                } else {
                    String::from("no")
                }
            }
            SettingsValue::Integer(value) => value.to_string(),
        })
        .unwrap_or_default()
}

fn set_field_value(values: &mut Vec<FieldValueEntry>, key: &str, value: SettingsValue) {
    clear_field_value(values, key);
    values.push(FieldValueEntry {
        key: key.to_string(),
        value,
    });
}

fn clear_field_value(values: &mut Vec<FieldValueEntry>, key: &str) {
    values.retain(|value| value.key != key);
}

fn parse_field_input(field: &FieldDefinition, value: &str) -> Option<SettingsValue> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    match field.value_kind {
        agent_runtime::FieldValueKind::Integer => {
            trimmed.parse::<i64>().ok().map(SettingsValue::Integer)
        }
        agent_runtime::FieldValueKind::Boolean => {
            let normalized = trimmed.to_ascii_lowercase();
            match normalized.as_str() {
                "true" | "yes" | "1" => Some(SettingsValue::Bool(true)),
                "false" | "no" | "0" => Some(SettingsValue::Bool(false)),
                _ => None,
            }
        }
        agent_runtime::FieldValueKind::String
        | agent_runtime::FieldValueKind::SecretString
        | agent_runtime::FieldValueKind::Url => Some(SettingsValue::String(trimmed.to_string())),
    }
}

fn is_boolean_field(field: &FieldDefinition) -> bool {
    matches!(field.value_kind, agent_runtime::FieldValueKind::Boolean)
}

impl App {
    fn update_chat_viewport(&mut self, height: u16) {
        self.state.chat_viewport_height = height;
        self.clamp_chat_scroll();
    }

    fn clamp_chat_scroll(&mut self) {
        if self.state.auto_scroll {
            return;
        }

        self.state.scroll = self.state.scroll.min(self.state.chat_max_scroll());
    }

    fn scroll_chat_by(&mut self, delta: i16) {
        self.state.auto_scroll = false;
        self.state.scroll = self
            .state
            .scroll
            .saturating_add_signed(delta)
            .min(self.state.chat_max_scroll());
        self.clear_chat_selection();
    }

    fn scroll_chat_to_top(&mut self) {
        self.state.auto_scroll = false;
        self.state.scroll = 0;
        self.clear_chat_selection();
    }

    fn scroll_chat_to_bottom(&mut self) {
        self.state.auto_scroll = true;
        self.clear_chat_selection();
    }

    pub fn new(runtime: RuntimeHandle, event_rx: Receiver<Event>) -> Self {
        let (runtime_tx, runtime_rx) = spawn_runtime_bridge(runtime);

        Self {
            event_rx,
            runtime_tx,
            runtime_rx,
            clipboard: None,
            state: AppState::default(),
            last_stream_refresh: None,
        }
    }

    pub fn run(&mut self, mut terminal: ratatui::DefaultTerminal) -> Result<()> {
        let mut needs_redraw = true;
        while !self.state.exit {
            needs_redraw |= self.process_events();
            let event_timeout = if needs_redraw {
                std::time::Duration::from_millis(0)
            } else {
                std::time::Duration::from_millis(100)
            };
            needs_redraw |= self.handle_events(event_timeout)?;

            if !needs_redraw {
                continue;
            }

            if self.state.mode == UiMode::Chat && self.state.tool_phase == ToolPhase::Deciding {
                terminal.hide_cursor()?;
            } else {
                terminal.show_cursor()?;
            }

            terminal.draw(|frame| {
                let area = frame.area();
                if self.state.mode == UiMode::Chat && self.state.tool_phase != ToolPhase::Deciding {
                    let input_height = bottom_panel_height(&self.state, area);
                    let layout = Layout::vertical([
                        Constraint::Min(area.height.saturating_sub(input_height + 1)),
                        Constraint::Length(1),
                        Constraint::Length(input_height),
                    ])
                    .flex(Flex::End);
                    let [chat_area, _, _] = layout.areas(area);
                    self.state.refresh_chat_render_cache(chat_area.width);
                    self.update_chat_viewport(chat_area.height);
                    let view = {
                        let cache = self.state.chat_render_cache.borrow();
                        ChatHistory::visible_view_from_sections(
                            &cache.sections,
                            cache.total_lines,
                            chat_area,
                            self.state.scroll,
                            self.state.auto_scroll,
                        )
                    };
                    self.state.visible_chat_view = Some(view);
                } else {
                    self.state.visible_chat_view = None;
                }

                frame.render_widget(&self.state, area);

                if self.state.mode == UiMode::Chat && self.state.tool_phase != ToolPhase::Deciding {
                    let input_height = bottom_panel_height(&self.state, area);
                    let layout = Layout::vertical([
                        Constraint::Min(area.height.saturating_sub(input_height + 1)),
                        Constraint::Length(1),
                        Constraint::Length(input_height),
                    ])
                    .flex(Flex::End);
                    let [_chat_area, _status_area, input_area] = layout.areas(area);

                    let (cursor_x, cursor_y) =
                        TextInput::new(&self.state.input, self.state.input_cursor)
                            .get_cursor_position(input_area);
                    frame.set_cursor_position((cursor_x, cursor_y));
                }
            })?;
            needs_redraw = false;
        }
        Ok(())
    }

    fn process_events(&mut self) -> bool {
        let mut changed = false;

        while let Ok(event) = self.event_rx.try_recv() {
            self.handle_runtime_event(event);
            changed = true;
        }

        while let Ok(response) = self.runtime_rx.try_recv() {
            self.handle_runtime_response(response);
            changed = true;
        }

        changed
    }

    fn handle_runtime_event(&mut self, event: Event) {
        match event {
            Event::ConfigLoaded => {
                self.state.config_loaded = true;
                self.state.status = String::from("Config loaded");
                self.request_sync();
            }
            Event::Error(msg) => {
                self.state.is_streaming = false;
                self.state.status = format!("Runtime error: {msg}");
            }
            Event::StreamStart { session_id, .. } => {
                self.request(RuntimeRequest::ListSessions);
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                self.state.is_streaming = true;
                self.last_stream_refresh = None;
                self.request(RuntimeRequest::GetCurrentTip { session_id });
            }
            Event::StreamChunk { session_id, .. } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                let now = Instant::now();
                let should_refresh = self
                    .last_stream_refresh
                    .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(50));
                if should_refresh {
                    self.last_stream_refresh = Some(now);
                    self.request(RuntimeRequest::GetCurrentTip {
                        session_id: session_id.clone(),
                    });
                    self.request(RuntimeRequest::GetChatHistory { session_id });
                }
            }
            Event::StreamComplete { session_id, .. } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.is_streaming = false;
                    self.last_stream_refresh = None;
                    self.request_sync_for_session(&session_id);
                    if self.state.tool_phase == ToolPhase::ExecutingBatch
                        && self.state.pending_tools.is_empty()
                    {
                        self.finish_tool_batch_execution();
                    }
                }
                self.request(RuntimeRequest::ListSessions);
            }
            Event::StreamError {
                session_id, error, ..
            } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.is_streaming = false;
                    self.last_stream_refresh = None;
                    self.state.status = format!("Stream error: {error}");
                    if self.state.tool_phase == ToolPhase::ExecutingBatch
                        && self.state.pending_tools.is_empty()
                    {
                        self.finish_tool_batch_execution();
                    }
                }
                self.request(RuntimeRequest::ListSessions);
            }
            Event::StreamCancelled { session_id, .. } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.is_streaming = false;
                    self.last_stream_refresh = None;
                    self.state.status = String::from("Stream cancelled");
                    self.request_sync_for_session(&session_id);
                    if self.state.tool_phase == ToolPhase::ExecutingBatch
                        && self.state.pending_tools.is_empty()
                    {
                        self.finish_tool_batch_execution();
                    }
                }
                self.request(RuntimeRequest::ListSessions);
            }
            Event::ContinuationFailed { session_id, error } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.is_streaming = false;
                    self.last_stream_refresh = None;
                    self.state.status = format!("Continuation failed: {error}");
                    self.request_sync_for_session(&session_id);
                    if self.state.tool_phase == ToolPhase::ExecutingBatch
                        && self.state.pending_tools.is_empty()
                    {
                        self.finish_tool_batch_execution();
                    }
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
            }
            Event::HistoryUpdated { session_id } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.clamp_chat_scroll();
                    self.request_sync_for_session(&session_id);
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
            }
            Event::OpenAiCodexAuthUpdated { status } => {
                self.apply_openai_codex_auth_status(map_openai_codex_auth_status(status));
                if self.state.mode == UiMode::ProvidersMenu
                    && matches!(self.state.providers_view, ProvidersView::Detail)
                    && pending_auth_target(&self.state.openai_codex_auth).is_none()
                {
                    self.state.status = String::from("OpenAI auth updated");
                }
            }
            Event::MessageComplete(_) => {}
            Event::ToolCallDetected {
                session_id,
                call_id,
                tool_id,
                args,
                description,
                risk_level,
                reasons,
                queue_order,
            } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    self.request(RuntimeRequest::ListSessions);
                    return;
                }

                let exists = self
                    .state
                    .pending_tools
                    .iter()
                    .any(|tool| tool.call_id == call_id);
                if !exists {
                    self.state.pending_tools.push(PendingTool {
                        call_id,
                        tool_id,
                        args,
                        description,
                        risk_level,
                        reasons,
                        approved: None,
                        queue_order,
                    });
                }
                self.sort_pending_tools();
                self.enter_tool_decision_phase();
                self.state.status =
                    format!("{} tool call(s) pending", self.state.pending_tools.len());
            }
            Event::ToolResultReady {
                session_id,
                call_id,
                tool_id,
                success,
                denied,
                output,
            } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state
                        .pending_tools
                        .retain(|tool| tool.call_id != call_id);
                    self.sort_pending_tools();
                    if !success || denied {
                        self.push_optimistic_tool_message(&call_id, &tool_id, &output, denied);
                    }
                    self.state.status = if denied {
                        format!("Tool denied: {tool_id}")
                    } else if success {
                        format!("Tool succeeded: {tool_id}")
                    } else {
                        format!("Tool failed: {tool_id}")
                    };
                    if self.state.pending_tools.is_empty()
                        && self.state.tool_phase == ToolPhase::ExecutingBatch
                        && !self.state.is_streaming
                    {
                        self.state.status = format!("Waiting for assistant after {tool_id}");
                    } else {
                        self.sync_tool_phase_from_pending_tools();
                    }
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
            }
        }
    }

    fn handle_runtime_response(&mut self, response: RuntimeResponse) {
        match response {
            RuntimeResponse::Models(Ok(models)) => {
                self.state.models_by_provider = models;
                self.ensure_selected_model();
            }
            RuntimeResponse::Models(Err(err)) => {
                self.state.status = format!("Failed loading models: {err}");
            }
            RuntimeResponse::AgentProfiles {
                session_id,
                result: Ok(state),
            } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                self.apply_agent_profiles_state(state);
            }
            RuntimeResponse::AgentProfiles {
                result: Err(err), ..
            } => {
                self.state.status = format!("Failed loading agent profiles: {err}");
            }
            RuntimeResponse::ProviderDefinitions(Ok(definitions)) => {
                self.state.provider_definitions = definitions;
            }
            RuntimeResponse::ProviderDefinitions(Err(err)) => {
                self.state.status = format!("Failed loading provider definitions: {err}");
            }
            RuntimeResponse::Settings(Ok(settings)) => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.settings_draft = Some(settings);
                self.state.settings_errors.clear();
                self.state.settings_focus = SettingsFocus::ProviderList;
                self.state.settings_provider_index = 0;
                self.state.settings_model_index = 0;
                self.state.settings_provider_field_index = 0;
                self.state.settings_model_field_index = 0;
                self.state.settings_editor = None;
                self.state.settings_editor_input.clear();
                self.state.settings_delete_armed = false;
                self.state.providers_view = ProvidersView::List;
                self.state.providers_advanced_focus = ProvidersAdvancedFocus::ProviderFields;
                self.state.connect_provider_search.clear();
                self.state.connect_provider_index = 0;
                self.state.mode = UiMode::ProvidersMenu;
                self.state.status = String::from("Providers loaded");
            }
            RuntimeResponse::Settings(Err(err)) => {
                self.state.status = format!("Failed loading settings: {err}");
            }
            RuntimeResponse::OpenAiCodexAuthStatus(result)
            | RuntimeResponse::StartOpenAiCodexBrowserLogin(result)
            | RuntimeResponse::StartOpenAiCodexDeviceCodeLogin(result)
            | RuntimeResponse::CancelOpenAiCodexLogin(result)
            | RuntimeResponse::LogoutOpenAiCodexAuth(result) => match result {
                Ok(status) => {
                    self.apply_openai_codex_auth_status(status);
                }
                Err(err) => {
                    self.state.status = format!("OpenAI auth failed: {err}");
                }
            },
            RuntimeResponse::CreateSession(Ok(session_id)) => {
                let draft_profile_id = self.state.selected_profile_id.clone();
                let pending_submit = self.state.pending_submit.take().map(|mut pending_submit| {
                    pending_submit.session_id = Some(session_id.clone());
                    pending_submit
                });
                self.reset_chat_session(Some(session_id.clone()), "Session ready");
                self.state.pending_submit = pending_submit;
                self.state.selected_profile_id = draft_profile_id.clone();
                self.request_sync_for_session(&session_id);

                if draft_profile_id.as_deref() != Some(DEFAULT_AGENT_PROFILE_ID)
                    && let Some(profile_id) = draft_profile_id
                {
                    self.request(RuntimeRequest::SetSessionProfile {
                        session_id,
                        profile_id,
                    });
                    return;
                }

                if let Some(pending_submit) = self.state.pending_submit.take() {
                    self.dispatch_send_message(
                        session_id,
                        pending_submit.message,
                        pending_submit.model_id,
                        pending_submit.provider_id,
                        false,
                    );
                }
            }
            RuntimeResponse::CreateSession(Err(err)) => {
                self.state.pending_submit = None;
                self.state.status = format!("Failed creating session: {err}");
            }
            RuntimeResponse::SetSessionProfile {
                profile_id,
                result: Ok(()),
            } => {
                self.state.selected_profile_id = Some(profile_id.clone());
                self.state.status = format!("Selected agent: {profile_id}");
                if let Some(session_id) = self.state.current_session_id.clone() {
                    self.request(RuntimeRequest::ListSessions);
                    self.request(RuntimeRequest::ListAgentProfiles { session_id });
                }
                self.state.mode = UiMode::Chat;

                if let Some(pending_submit) = self.state.pending_submit.take()
                    && let Some(session_id) = pending_submit.session_id
                {
                    self.dispatch_send_message(
                        session_id,
                        pending_submit.message,
                        pending_submit.model_id,
                        pending_submit.provider_id,
                        false,
                    );
                }
            }
            RuntimeResponse::SetSessionProfile {
                result: Err(err), ..
            } => {
                self.state.pending_submit = None;
                self.state.status = format!("Failed changing agent: {err}");
            }
            RuntimeResponse::SendMessage(Ok(())) => {}
            RuntimeResponse::SendMessage(Err(err)) => {
                if !self.state.optimistic_messages.is_empty() {
                    self.state.optimistic_messages.remove(0);
                    self.update_queued_status();
                    self.invalidate_chat_cache();
                }
                self.state.is_streaming = false;
                self.state.status = format!("Send failed: {err}");
            }
            RuntimeResponse::SaveSettings(Ok(())) => {
                self.state.settings_errors.clear();
                self.state.settings_delete_armed = false;
                self.state.settings_editor = None;
                self.state.settings_editor_input.clear();
                self.state.status = String::from("Providers saved");
                self.request(RuntimeRequest::ListModels);
            }
            RuntimeResponse::SaveSettings(Err(err)) => {
                self.state.settings_errors = parse_settings_errors(&err);
                self.state.status = format!("Failed saving settings: {err}");
            }
            RuntimeResponse::ChatHistory { session_id, result } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }

                match result {
                    Ok(history) => {
                        self.state.chat_history = history;
                        self.invalidate_chat_cache();
                        self.reconcile_optimistic_messages();
                        self.reconcile_optimistic_tool_messages();
                        self.clamp_chat_scroll();
                    }
                    Err(err) => {
                        self.state.status = format!("Failed loading history: {err}");
                    }
                }
            }
            RuntimeResponse::CurrentTip { session_id, result } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }

                match result {
                    Ok(tip) => {
                        if self.state.current_tip_id != tip {
                            self.state.current_tip_id = tip;
                            self.invalidate_chat_cache();
                            self.reconcile_optimistic_messages();
                            self.reconcile_optimistic_tool_messages();
                            self.clamp_chat_scroll();
                        }
                    }
                    Err(err) => {
                        self.state.status = format!("Failed loading tip: {err}");
                    }
                }
            }
            RuntimeResponse::PendingTools { session_id, result } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }

                match result {
                    Ok(pending_tools) => {
                        let should_auto_start_execution = self.state.tool_phase == ToolPhase::Idle;
                        self.state.pending_tools = pending_tools
                            .into_iter()
                            .map(|tool| PendingTool {
                                call_id: tool.call_id,
                                tool_id: tool.tool_id,
                                args: tool.args,
                                description: tool.description,
                                risk_level: tool.risk_level,
                                reasons: tool.reasons,
                                approved: tool.approved,
                                queue_order: tool.queue_order,
                            })
                            .collect();
                        self.sync_tool_phase_from_pending_tools();
                        if should_auto_start_execution
                            && self.state.tool_phase == ToolPhase::ExecutingBatch
                            && !self.state.tool_batch_execution_started
                            && !self.has_undecided_tools()
                            && !self.state.pending_tools.is_empty()
                        {
                            self.maybe_start_tool_batch_execution();
                        }
                    }
                    Err(err) => {
                        self.state.status = format!("Failed loading pending tools: {err}");
                    }
                }
            }
            RuntimeResponse::LoadSession {
                session_id,
                result: Ok(true),
            } => {
                self.reset_chat_session(Some(session_id), "Session loaded");
                self.request_sync();
            }
            RuntimeResponse::LoadSession {
                result: Ok(false), ..
            } => {
                self.state.status = String::from("Session not found");
            }
            RuntimeResponse::LoadSession {
                result: Err(err), ..
            } => {
                self.state.status = format!("Failed to load session: {err}");
            }
            RuntimeResponse::Sessions(Ok(sessions)) => {
                self.state.sessions = sessions;
                if self.state.sessions_menu_index > self.state.sessions.len() {
                    self.state.sessions_menu_index = self.state.sessions.len();
                }
                self.sync_current_session_profile_from_sessions();
            }
            RuntimeResponse::Sessions(Err(err)) => {
                self.state.status = format!("Failed loading sessions: {err}");
            }
            RuntimeResponse::DeleteSession {
                session_id,
                result: Ok(()),
            } => {
                self.state.status = String::from("Session deleted");
                self.state.sessions.retain(|s| s.id != session_id);
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.reset_chat_session(None, "Session deleted");
                }
            }
            RuntimeResponse::DeleteSession {
                result: Err(err), ..
            } => {
                self.state.status = format!("Failed deleting session: {err}");
            }
            RuntimeResponse::ApproveTool {
                call_id,
                result: Ok(()),
            } => {
                self.set_tool_approval(&call_id, Some(true));
                if self.has_undecided_tools() {
                    self.enter_tool_decision_phase();
                } else {
                    self.state.tool_phase = ToolPhase::ExecutingBatch;
                    self.maybe_start_tool_batch_execution();
                }
            }
            RuntimeResponse::ApproveTool {
                result: Err(err), ..
            } => {
                self.state.status = format!("Failed approving tool: {err}");
            }
            RuntimeResponse::DenyTool {
                call_id,
                result: Ok(()),
            } => {
                self.set_tool_approval(&call_id, Some(false));
                if self.has_undecided_tools() {
                    self.enter_tool_decision_phase();
                } else {
                    self.state.tool_phase = ToolPhase::ExecutingBatch;
                    self.maybe_start_tool_batch_execution();
                }
            }
            RuntimeResponse::DenyTool {
                result: Err(err), ..
            } => {
                self.state.status = format!("Failed denying tool: {err}");
            }
            RuntimeResponse::CancelStream(Ok(true)) => {}
            RuntimeResponse::CancelStream(Ok(false)) => {
                self.state.status = String::from("No active stream to cancel");
            }
            RuntimeResponse::CancelStream(Err(err)) => {
                self.state.status = format!("Failed cancelling stream: {err}");
            }
            RuntimeResponse::ExecuteApprovedTools(Ok(())) => {
                self.state.status = String::from("Executing decided tool calls");
            }
            RuntimeResponse::ExecuteApprovedTools(Err(err)) => {
                self.state.tool_batch_execution_started = false;
                self.state.status = format!("Failed executing tools: {err}");
            }
        }
    }

    fn handle_events(&mut self, timeout: std::time::Duration) -> Result<bool> {
        if !event::poll(timeout)? {
            return Ok(false);
        }

        let mut changed = false;
        loop {
            if self.handle_terminal_event(event::read()?) {
                changed = true;
            }

            if !event::poll(std::time::Duration::from_millis(0))? {
                break;
            }
        }

        Ok(changed)
    }

    fn handle_terminal_event(&mut self, event: CrosstermEvent) -> bool {
        match event {
            CrosstermEvent::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                self.handle_key_event(key_event);
                true
            }
            CrosstermEvent::Mouse(mouse_event) => {
                self.handle_mouse_event(mouse_event);
                true
            }
            CrosstermEvent::Paste(text) => {
                self.handle_paste(text);
                true
            }
            CrosstermEvent::Resize(_, _) => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                true
            }
            _ => false,
        }
    }

    fn handle_mouse_event(&mut self, mouse_event: MouseEvent) {
        if self.state.mode != UiMode::Chat || self.state.tool_phase == ToolPhase::Deciding {
            return;
        }

        match mouse_event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(position) = self.hit_test_chat_cell(mouse_event.column, mouse_event.row)
                {
                    self.state.selection = Some(ChatSelection {
                        anchor: position,
                        focus: position,
                    });
                } else {
                    self.clear_chat_selection();
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(position) = self.hit_test_chat_cell(mouse_event.column, mouse_event.row)
                    && let Some(selection) = self.state.selection.as_mut()
                {
                    selection.focus = position;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(position) = self.hit_test_chat_cell(mouse_event.column, mouse_event.row)
                    && let Some(selection) = self.state.selection.as_mut()
                {
                    selection.focus = position;
                }
            }
            MouseEventKind::ScrollUp => {
                self.scroll_chat_by(-1);
            }
            MouseEventKind::ScrollDown => {
                self.scroll_chat_by(1);
            }
            _ => {}
        }
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if self.state.mode == UiMode::Chat && self.state.tool_phase == ToolPhase::Deciding {
            if matches!(key_event.code, KeyCode::Esc) {
                return;
            }
            self.handle_tool_approval_key_event(key_event);
            return;
        }

        if matches!(key_event.code, KeyCode::Esc) {
            if self.state.mode == UiMode::Chat && self.command_popup_visible() {
                self.state.command_popup_dismissed = true;
                self.reset_completion_cycle();
                return;
            }
            if self.state.mode == UiMode::Chat && self.state.is_streaming {
                if let Some(session_id) = &self.state.current_session_id {
                    self.request(RuntimeRequest::CancelStream {
                        session_id: session_id.clone(),
                    });
                }
                return;
            }
            if self.state.mode == UiMode::ProvidersMenu {
                self.handle_providers_escape();
                return;
            }
            self.clear_chat_selection();
            self.state.visible_chat_view = None;
            self.state.mode = UiMode::Chat;
            return;
        }

        match self.state.mode {
            UiMode::Chat => self.handle_chat_key_event(key_event),
            UiMode::AgentMenu => self.handle_agent_menu_key_event(key_event),
            UiMode::ModelMenu => self.handle_model_menu_key_event(key_event),
            UiMode::ProvidersMenu => self.handle_providers_key_event(key_event),
            UiMode::SessionsMenu => self.handle_sessions_menu_key_event(key_event),
            UiMode::Help => {
                if matches!(key_event.code, KeyCode::Enter | KeyCode::Char('q')) {
                    self.clear_chat_selection();
                    self.state.mode = UiMode::Chat;
                }
            }
        }
    }

    fn handle_chat_key_event(&mut self, key_event: KeyEvent) {
        if is_ctrl_c(key_event) {
            self.handle_ctrl_c();
            return;
        }

        self.state.ctrl_c_exit_armed = false;

        if is_copy_shortcut(key_event) {
            self.copy_selection_to_clipboard();
            return;
        }

        match key_event.code {
            KeyCode::Enter => {
                if key_event.modifiers.contains(KeyModifiers::SHIFT) {
                    self.insert_input_char('\n');
                    self.reset_completion_cycle();
                } else if !self.execute_current_command_suggestion() {
                    self.reset_completion_cycle();
                    self.handle_submit();
                }
            }
            KeyCode::Tab => {
                self.cycle_command_suggestion(true);
            }
            KeyCode::BackTab => {
                self.cycle_command_suggestion(false);
            }
            KeyCode::Char(c) => {
                self.insert_input_char(c);
                if active_command_prefix(&self.state.input).is_none() {
                    self.state.command_popup_dismissed = false;
                }
                self.reset_completion_cycle();
            }
            KeyCode::Backspace => {
                self.backspace_input_char();
                if active_command_prefix(&self.state.input).is_none() {
                    self.state.command_popup_dismissed = false;
                }
                self.reset_completion_cycle();
            }
            KeyCode::Up => {
                if active_command_prefix(&self.state.input).is_some() {
                    self.cycle_command_suggestion(false);
                } else {
                    self.state.input_cursor = 0;
                }
            }
            KeyCode::Down => {
                if active_command_prefix(&self.state.input).is_some() {
                    self.cycle_command_suggestion(true);
                } else {
                    self.state.input_cursor = self.state.input.len();
                }
            }
            KeyCode::Left => {
                self.move_input_cursor_left();
            }
            KeyCode::Right => {
                self.move_input_cursor_right();
            }
            KeyCode::PageUp => {
                self.scroll_chat_by(-10);
            }
            KeyCode::PageDown => {
                self.scroll_chat_by(10);
            }
            KeyCode::Home => {
                self.scroll_chat_to_top();
            }
            KeyCode::End => {
                self.scroll_chat_to_bottom();
            }
            _ => {}
        }
    }

    fn handle_paste(&mut self, text: String) {
        if self.state.mode != UiMode::Chat
            || self.state.tool_phase == ToolPhase::Deciding
            || text.is_empty()
        {
            return;
        }

        self.insert_input_text(&text);
        if active_command_prefix(&self.state.input).is_none() {
            self.state.command_popup_dismissed = false;
        }
        self.reset_completion_cycle();
    }

    fn handle_ctrl_c(&mut self) {
        if self.state.ctrl_c_exit_armed {
            self.state.exit = true;
            return;
        }

        self.clear_chat_transient_state();
        self.state.ctrl_c_exit_armed = true;
        self.state.status = String::from("Cleared input. Press Ctrl+C again to exit");
    }

    fn clear_chat_transient_state(&mut self) {
        self.state.input.clear();
        self.state.input_cursor = 0;
        self.clear_chat_selection();
        self.state.visible_chat_view = None;
        self.state.command_popup_dismissed = false;
        self.reset_completion_cycle();
    }

    fn reset_completion_cycle(&mut self) {
        self.state.command_completion_prefix = None;
        self.state.command_completion_index = 0;
    }

    fn cycle_command_suggestion(&mut self, forward: bool) {
        if self.state.command_popup_dismissed {
            return;
        }
        let Some(prefix) = active_command_prefix(&self.state.input) else {
            return;
        };
        let matches = slash_command_matches(prefix);
        if matches.is_empty() {
            self.state.status = format!("No command matches '/{prefix}'");
            return;
        }

        let next_index = if self.state.command_completion_prefix.as_deref() == Some(prefix) {
            if forward {
                (self.state.command_completion_index + 1) % matches.len()
            } else if self.state.command_completion_index == 0 {
                matches.len() - 1
            } else {
                self.state.command_completion_index - 1
            }
        } else if forward {
            usize::from(matches.len() > 1)
        } else {
            matches.len() - 1
        };

        self.state.command_completion_prefix = Some(prefix.to_string());
        self.state.command_completion_index = next_index;
    }

    fn execute_current_command_suggestion(&mut self) -> bool {
        if self.state.command_popup_dismissed {
            return false;
        }
        let Some(prefix) = active_command_prefix(&self.state.input) else {
            return false;
        };
        let matches = slash_command_matches(prefix);
        if matches.is_empty() {
            return false;
        }

        let selected_idx = if self.state.command_completion_prefix.as_deref() == Some(prefix) {
            self.state.command_completion_index.min(matches.len() - 1)
        } else {
            0
        };

        let command = matches[selected_idx].0;
        self.state.input.clear();
        self.state.input_cursor = 0;
        self.state.command_popup_dismissed = false;
        self.reset_completion_cycle();
        self.handle_command(command);
        true
    }

    fn command_popup_visible(&self) -> bool {
        if self.state.command_popup_dismissed {
            return false;
        }

        active_command_prefix(&self.state.input)
            .map(slash_command_matches)
            .is_some_and(|matches| !matches.is_empty())
    }

    fn handle_model_menu_key_event(&mut self, key_event: KeyEvent) {
        let models = self.flatten_models();
        let len = models.len();

        match key_event.code {
            KeyCode::Up => {
                if len > 0 {
                    self.state.model_menu_index =
                        model_menu_previous_index(self.state.model_menu_index, len);
                }
            }
            KeyCode::Down => {
                if len > 0 {
                    self.state.model_menu_index =
                        model_menu_next_index(self.state.model_menu_index, len);
                }
            }
            KeyCode::Enter => {
                if let Some((provider_id, model)) = models.get(self.state.model_menu_index) {
                    self.state.selected_provider_id = Some(provider_id.clone());
                    self.state.selected_model_id = Some(model.id.clone());
                    self.state.status = format!("Selected model: {} / {}", provider_id, model.name);
                    self.state.mode = UiMode::Chat;
                }
            }
            _ => {}
        }
    }

    fn handle_agent_menu_key_event(&mut self, key_event: KeyEvent) {
        let len = self.state.agent_profiles.len();

        match key_event.code {
            KeyCode::Up => {
                if len > 0 {
                    self.state.agent_menu_index =
                        model_menu_previous_index(self.state.agent_menu_index, len);
                }
            }
            KeyCode::Down => {
                if len > 0 {
                    self.state.agent_menu_index =
                        model_menu_next_index(self.state.agent_menu_index, len);
                }
            }
            KeyCode::Enter => {
                if self.state.profile_locked {
                    self.state.status =
                        String::from("Cannot change agent while the current turn is active");
                    self.state.mode = UiMode::Chat;
                    return;
                }
                if let Some(profile) = self.state.agent_profiles.get(self.state.agent_menu_index) {
                    if let Some(session_id) = self.state.current_session_id.clone() {
                        self.request(RuntimeRequest::SetSessionProfile {
                            session_id,
                            profile_id: profile.id.clone(),
                        });
                    } else {
                        self.state.selected_profile_id = Some(profile.id.clone());
                        self.state.status = format!("Selected agent: {}", profile.id);
                        self.state.mode = UiMode::Chat;
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_sessions_menu_key_event(&mut self, key_event: KeyEvent) {
        let total = self.state.sessions.len() + 1;

        match key_event.code {
            KeyCode::Up => {
                if total > 0 {
                    self.state.sessions_menu_index =
                        (self.state.sessions_menu_index + total - 1) % total;
                }
            }
            KeyCode::Down => {
                if total > 0 {
                    self.state.sessions_menu_index = (self.state.sessions_menu_index + 1) % total;
                }
            }
            KeyCode::Enter => {
                if self.state.sessions_menu_index == 0 {
                    self.start_new_chat();
                } else if let Some(session) = self
                    .state
                    .sessions
                    .get(self.state.sessions_menu_index.saturating_sub(1))
                {
                    self.request(RuntimeRequest::LoadSession {
                        session_id: session.id.clone(),
                    });
                }
            }
            KeyCode::Char('x') => {
                if self.state.sessions_menu_index > 0
                    && let Some(session) = self
                        .state
                        .sessions
                        .get(self.state.sessions_menu_index.saturating_sub(1))
                {
                    self.request(RuntimeRequest::DeleteSession {
                        session_id: session.id.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    fn handle_tool_approval_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Left | KeyCode::BackTab => self.select_previous_tool_action(),
            KeyCode::Right | KeyCode::Tab => self.select_next_tool_action(),
            KeyCode::Enter => self.confirm_current_tool_action(),
            KeyCode::Char('a') => self.submit_tool_decision(true),
            KeyCode::Char('d') => self.submit_tool_decision(false),
            _ => {}
        }
    }

    fn handle_providers_key_event(&mut self, key_event: KeyEvent) {
        if self.state.settings_editor.is_some() {
            self.handle_settings_editor_key_event(key_event);
            return;
        }

        match self.state.providers_view {
            ProvidersView::List => self.handle_provider_list_key_event(key_event),
            ProvidersView::Connect => self.handle_connect_provider_key_event(key_event),
            ProvidersView::Detail => self.handle_provider_detail_key_event(key_event),
            ProvidersView::Advanced => self.handle_advanced_provider_key_event(key_event),
        }
    }

    fn handle_settings_editor_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Enter => self.commit_settings_editor(),
            KeyCode::Backspace => {
                self.state.settings_editor_input.pop();
            }
            KeyCode::Char(c) => {
                self.state.settings_editor_input.push(c);
            }
            _ => {}
        }
    }

    fn move_settings_selection(&mut self, delta: isize) {
        self.state.settings_delete_armed = false;
        match self.state.settings_focus {
            SettingsFocus::ProviderList => {
                let len = self
                    .state
                    .settings_draft
                    .as_ref()
                    .map_or(0, |draft| draft.providers.len());
                self.state.settings_provider_index =
                    adjust_index(self.state.settings_provider_index, len, delta);
                self.state.settings_model_index = 0;
            }
            SettingsFocus::ProviderForm => {
                let len = self.current_provider_fields().len();
                self.state.settings_provider_field_index =
                    adjust_index(self.state.settings_provider_field_index, len, delta);
            }
            SettingsFocus::ModelList => {
                let len = self.current_model_indices().len();
                self.state.settings_model_index =
                    adjust_index(self.state.settings_model_index, len, delta);
            }
            SettingsFocus::ModelForm => {
                let len = self.current_model_fields().len();
                self.state.settings_model_field_index =
                    adjust_index(self.state.settings_model_field_index, len, delta);
            }
        }
    }

    fn adjust_settings_field(&mut self, forward: bool) {
        match self.state.settings_focus {
            SettingsFocus::ProviderForm => {
                if self.current_provider_field() == Some(SettingsProviderField::TypeId) {
                    self.cycle_provider_type(forward);
                } else if self.current_provider_field().is_some_and(|field| {
                    matches!(field, SettingsProviderField::Value(ref key) if self.current_provider_field_definition(key).is_some_and(is_boolean_field))
                }) {
                    self.toggle_settings_field();
                }
            }
            SettingsFocus::ModelForm | SettingsFocus::ProviderList | SettingsFocus::ModelList => {}
        }
    }

    fn activate_settings_field(&mut self) {
        self.state.settings_delete_armed = false;
        match self.state.settings_focus {
            SettingsFocus::ProviderForm => match self.current_provider_field() {
                Some(SettingsProviderField::TypeId) => self.cycle_provider_type(true),
                Some(SettingsProviderField::Value(ref key))
                    if self
                        .current_provider_field_definition(key)
                        .is_some_and(is_boolean_field) =>
                {
                    self.toggle_settings_field()
                }
                Some(field) => self.start_provider_editor(field),
                None => {}
            },
            SettingsFocus::ModelForm => {
                if let Some(field) = self.current_model_field() {
                    self.start_model_editor(field);
                }
            }
            SettingsFocus::ProviderList | SettingsFocus::ModelList => {}
        }
    }

    fn toggle_settings_field(&mut self) {
        let provider_index = self.state.settings_provider_index;
        let Some(SettingsProviderField::Value(key)) = self.current_provider_field() else {
            return;
        };
        let mut changed = false;
        if let Some(provider) = self
            .state
            .settings_draft
            .as_mut()
            .and_then(|draft| draft.providers.get_mut(provider_index))
        {
            let current = provider
                .values
                .iter()
                .find(|value| value.key == key)
                .and_then(|value| match value.value {
                    SettingsValue::Bool(value) => Some(value),
                    SettingsValue::String(_) | SettingsValue::Integer(_) => None,
                })
                .unwrap_or(false);
            set_field_value(&mut provider.values, &key, SettingsValue::Bool(!current));
            changed = true;
        }
        if changed {
            self.save_settings_draft();
        }
    }

    fn add_settings_item(&mut self) {
        self.state.settings_delete_armed = false;
        let mut changed = false;
        match self.state.settings_focus {
            SettingsFocus::ProviderList | SettingsFocus::ProviderForm => {
                if let Some(draft) = self.state.settings_draft.as_mut() {
                    let Some(definition) = self.state.provider_definitions.first() else {
                        self.state.status = String::from("No provider definitions registered");
                        return;
                    };
                    let next_index = draft.providers.len();
                    draft.providers.push(ProviderSettings {
                        id: if next_index == 0 {
                            definition.default_provider_id_prefix.clone()
                        } else {
                            format!(
                                "{}-{}",
                                definition.default_provider_id_prefix,
                                next_index + 1
                            )
                        },
                        type_id: definition.type_id.clone(),
                        values: default_values(&definition.provider_fields),
                    });
                    self.state.settings_provider_index = next_index;
                    self.state.settings_model_index = 0;
                    self.state.status = String::from("Added provider");
                    changed = true;
                }
            }
            SettingsFocus::ModelList | SettingsFocus::ModelForm => {
                let provider_id = self.current_provider().map(|provider| provider.id.clone());
                let model_fields = self
                    .current_provider_definition()
                    .map(|definition| definition.model_fields.clone())
                    .unwrap_or_default();
                if let (Some(draft), Some(provider_id)) =
                    (self.state.settings_draft.as_mut(), provider_id)
                {
                    let next_count = draft
                        .models
                        .iter()
                        .filter(|model| model.provider_id == provider_id)
                        .count();
                    draft.models.push(ModelSettings {
                        id: format!("model-{}", next_count + 1),
                        provider_id,
                        values: default_values(&model_fields),
                    });
                    self.state.settings_model_index = next_count;
                    self.state.status = String::from("Added model");
                    changed = true;
                }
            }
        }
        if changed {
            self.save_settings_draft();
        }
    }

    fn delete_settings_item(&mut self) {
        if !self.state.settings_delete_armed {
            self.state.settings_delete_armed = true;
            self.state.status = String::from("Press x again to confirm delete");
            return;
        }

        self.state.settings_delete_armed = false;
        let mut changed = false;
        match self.state.settings_focus {
            SettingsFocus::ProviderList | SettingsFocus::ProviderForm => {
                let provider_id = self.current_provider().map(|provider| provider.id.clone());
                if let (Some(draft), Some(provider_id)) =
                    (self.state.settings_draft.as_mut(), provider_id)
                {
                    draft
                        .providers
                        .retain(|provider| provider.id != provider_id);
                    draft
                        .models
                        .retain(|model| model.provider_id != provider_id);
                    self.state.settings_provider_index = self
                        .state
                        .settings_provider_index
                        .saturating_sub(1)
                        .min(draft.providers.len().saturating_sub(1));
                    self.state.settings_model_index = 0;
                    self.state.status = String::from("Deleted provider");
                    changed = true;
                }
            }
            SettingsFocus::ModelList | SettingsFocus::ModelForm => {
                let selected = self
                    .current_model()
                    .map(|model| (model.provider_id.clone(), model.id.clone()));
                if let (Some(draft), Some((provider_id, model_id))) =
                    (self.state.settings_draft.as_mut(), selected)
                {
                    draft.models.retain(|model| {
                        !(model.provider_id == provider_id && model.id == model_id)
                    });
                    self.state.settings_model_index =
                        self.state.settings_model_index.saturating_sub(1);
                    self.state.status = String::from("Deleted model");
                    changed = true;
                }
            }
        }
        if changed {
            self.save_settings_draft();
        }
    }

    fn save_settings_draft(&mut self) {
        if let Some(settings) = self.state.settings_draft.clone() {
            self.request(RuntimeRequest::SaveSettings { settings });
        }
    }

    fn start_provider_editor(&mut self, field: SettingsProviderField) {
        let Some(provider) = self.current_provider() else {
            return;
        };
        let value = match &field {
            SettingsProviderField::Id => provider.id.clone(),
            SettingsProviderField::TypeId => return,
            SettingsProviderField::Value(key) => {
                if self
                    .current_provider_field_definition(key)
                    .is_some_and(is_boolean_field)
                {
                    return;
                }
                field_value_display(&provider.values, key)
            }
        };
        self.state.settings_editor = Some(ActiveSettingsEditor::Provider(field));
        self.state.settings_editor_input = value;
    }

    fn start_model_editor(&mut self, field: SettingsModelField) {
        let Some(model) = self.current_model() else {
            return;
        };
        let value = match &field {
            SettingsModelField::Id => model.id.clone(),
            SettingsModelField::Value(key) => field_value_display(&model.values, key),
        };
        self.state.settings_editor = Some(ActiveSettingsEditor::Model(field));
        self.state.settings_editor_input = value;
    }

    fn commit_settings_editor(&mut self) {
        let Some(editor) = self.state.settings_editor.take() else {
            return;
        };
        let value = self.state.settings_editor_input.trim().to_string();
        let mut changed = false;

        match editor {
            ActiveSettingsEditor::Provider(field) => {
                let provider_index = self.state.settings_provider_index;
                let provider_field_definition = match &field {
                    SettingsProviderField::Value(key) => {
                        self.current_provider_field_definition(key).cloned()
                    }
                    SettingsProviderField::Id | SettingsProviderField::TypeId => None,
                };
                if let Some(draft) = self.state.settings_draft.as_mut()
                    && let Some(provider) = draft.providers.get_mut(provider_index)
                {
                    match field {
                        SettingsProviderField::Id => {
                            let previous_id = provider.id.clone();
                            provider.id = value.clone();
                            for model in &mut draft.models {
                                if model.provider_id == previous_id {
                                    model.provider_id = value.clone();
                                }
                            }
                            changed = true;
                        }
                        SettingsProviderField::TypeId => {}
                        SettingsProviderField::Value(key) => {
                            if let Some(definition) = provider_field_definition
                                && let Some(next_value) =
                                    parse_field_input(&definition, value.as_str())
                            {
                                set_field_value(&mut provider.values, &key, next_value);
                            } else {
                                clear_field_value(&mut provider.values, &key);
                            }
                            changed = true;
                        }
                    }
                }
            }
            ActiveSettingsEditor::Model(field) => {
                let model_field_definition = match &field {
                    SettingsModelField::Value(key) => {
                        self.current_model_field_definition(key).cloned()
                    }
                    SettingsModelField::Id => None,
                };
                if let Some(global_index) = self.current_model_global_index()
                    && let Some(model) = self
                        .state
                        .settings_draft
                        .as_mut()
                        .and_then(|draft| draft.models.get_mut(global_index))
                {
                    match field {
                        SettingsModelField::Id => {
                            model.id = value;
                            changed = true;
                        }
                        SettingsModelField::Value(key) => {
                            if let Some(definition) = model_field_definition
                                && let Some(next_value) =
                                    parse_field_input(&definition, value.as_str())
                            {
                                set_field_value(&mut model.values, &key, next_value);
                            } else {
                                clear_field_value(&mut model.values, &key);
                            }
                            changed = true;
                        }
                    }
                }
            }
        }

        self.state.settings_editor_input.clear();
        if changed {
            self.save_settings_draft();
        }
    }

    fn cycle_provider_type(&mut self, forward: bool) {
        let provider_index = self.state.settings_provider_index;
        let Some(provider) = self
            .state
            .settings_draft
            .as_mut()
            .and_then(|draft| draft.providers.get_mut(provider_index))
        else {
            return;
        };
        if self.state.provider_definitions.is_empty() {
            return;
        }
        let len = self.state.provider_definitions.len();
        let current_index = self
            .state
            .provider_definitions
            .iter()
            .position(|definition| definition.type_id == provider.type_id)
            .unwrap_or(0);
        let next_index = if forward {
            (current_index + 1) % len
        } else {
            (current_index + len - 1) % len
        };
        if let Some(definition) = self.state.provider_definitions.get(next_index) {
            provider.type_id = definition.type_id.clone();
            provider.values = merge_values(&definition.provider_fields, &provider.values);
            self.save_settings_draft();
        }
    }

    fn current_provider(&self) -> Option<&ProviderSettings> {
        self.state
            .settings_draft
            .as_ref()
            .and_then(|draft| draft.providers.get(self.state.settings_provider_index))
    }

    pub(super) fn current_provider_definition(&self) -> Option<&ProviderDefinition> {
        let provider = self.current_provider()?;
        self.state
            .provider_definitions
            .iter()
            .find(|definition| definition.type_id == provider.type_id)
    }

    fn current_provider_fields(&self) -> Vec<SettingsProviderField> {
        let mut fields = vec![SettingsProviderField::Id, SettingsProviderField::TypeId];
        if let Some(definition) = self.current_provider_definition() {
            fields.extend(
                definition
                    .provider_fields
                    .iter()
                    .map(|field| SettingsProviderField::Value(field.key.clone())),
            );
        }
        fields
    }

    fn current_provider_field(&self) -> Option<SettingsProviderField> {
        self.current_provider_fields()
            .get(self.state.settings_provider_field_index)
            .cloned()
    }

    pub(super) fn current_provider_field_definition(&self, key: &str) -> Option<&FieldDefinition> {
        self.current_provider_definition()?
            .provider_fields
            .iter()
            .find(|field| field.key == key)
    }

    fn current_model_indices(&self) -> Vec<usize> {
        let Some(provider) = self.current_provider() else {
            return Vec::new();
        };
        self.state
            .settings_draft
            .as_ref()
            .map(|draft| {
                draft
                    .models
                    .iter()
                    .enumerate()
                    .filter_map(|(index, model)| {
                        (model.provider_id == provider.id).then_some(index)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn current_model_global_index(&self) -> Option<usize> {
        self.current_model_indices()
            .get(self.state.settings_model_index)
            .copied()
    }

    fn current_model(&self) -> Option<&ModelSettings> {
        let index = self.current_model_global_index()?;
        self.state
            .settings_draft
            .as_ref()
            .and_then(|draft| draft.models.get(index))
    }

    fn current_model_fields(&self) -> Vec<SettingsModelField> {
        let mut fields = vec![SettingsModelField::Id];
        if let Some(definition) = self.current_provider_definition() {
            fields.extend(
                definition
                    .model_fields
                    .iter()
                    .map(|field| SettingsModelField::Value(field.key.clone())),
            );
        }
        fields
    }

    pub(super) fn current_model_field_definition(&self, key: &str) -> Option<&FieldDefinition> {
        self.current_provider_definition()?
            .model_fields
            .iter()
            .find(|field| field.key == key)
    }

    fn current_model_field(&self) -> Option<SettingsModelField> {
        self.current_model_fields()
            .get(self.state.settings_model_field_index)
            .cloned()
    }

    fn handle_submit(&mut self) {
        let raw_input = self.state.input.trim().to_string();
        if raw_input.is_empty() {
            return;
        }

        let command_popup_dismissed = self.state.command_popup_dismissed;
        self.state.command_popup_dismissed = false;

        if !command_popup_dismissed
            && let Some(command) = raw_input.strip_prefix('/')
            && (is_known_slash_command(command) || command.trim() == "settings")
        {
            self.handle_command(command.trim());
            return;
        }

        self.state.input.clear();
        self.state.input_cursor = 0;

        if !self.state.config_loaded {
            self.state.status = String::from("Config not loaded yet");
            return;
        }

        let Some(provider_id) = self.state.selected_provider_id.clone() else {
            self.state.status = String::from("No provider selected. Use /model");
            return;
        };
        let Some(model_id) = self.state.selected_model_id.clone() else {
            self.state.status = String::from("No model selected. Use /model");
            return;
        };
        if self.state.selected_profile_id.is_none() {
            self.state.status = String::from("No agent selected. Use /agent");
            return;
        }

        let is_queueing = self.state.is_streaming
            || self.state.tool_phase == ToolPhase::ExecutingBatch
            || !self.state.pending_tools.is_empty();

        if let Some(session_id) = self.state.current_session_id.clone() {
            self.dispatch_send_message(session_id, raw_input, model_id, provider_id, is_queueing);
            return;
        }

        self.state.pending_submit = Some(PendingSubmit {
            session_id: None,
            message: raw_input,
            model_id,
            provider_id,
        });
        self.state.status = String::from("Creating session");
        self.request(RuntimeRequest::CreateSession);
    }

    fn handle_command(&mut self, command_line: &str) {
        let mut parts = command_line.split_whitespace();
        let Some(command) = parts.next() else {
            self.state.status = String::from("Empty command. Use /help");
            return;
        };

        match command {
            "quit" => {
                self.state.exit = true;
            }
            "model" => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::ModelMenu;
                self.request(RuntimeRequest::ListModels);
            }
            "agent" => {
                if self.state.profile_locked {
                    self.state.status =
                        String::from("Cannot change agent while the current turn is active");
                    return;
                }
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::AgentMenu;
                if let Some(session_id) = self.state.current_session_id.clone() {
                    self.request(RuntimeRequest::ListAgentProfiles { session_id });
                } else {
                    self.state.agent_profiles = default_agent_profiles();
                    self.state.agent_profile_warnings.clear();
                    if let Some(selected_profile_id) = self.state.selected_profile_id.as_ref()
                        && let Some(index) = self
                            .state
                            .agent_profiles
                            .iter()
                            .position(|profile| &profile.id == selected_profile_id)
                    {
                        self.state.agent_menu_index = index;
                    } else {
                        self.state.agent_menu_index = 0;
                    }
                }
            }
            "settings" => {
                self.state.status = String::from("Unknown command: /settings. Use /providers");
            }
            "providers" => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::ProvidersMenu;
                self.state.providers_view = ProvidersView::List;
                self.state.status = String::from("Loading providers");
                self.request(RuntimeRequest::ListProviderDefinitions);
                self.request(RuntimeRequest::GetSettings);
                self.request(RuntimeRequest::GetOpenAiCodexAuthStatus);
            }
            "sessions" => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::SessionsMenu;
                self.request(RuntimeRequest::ListSessions);
            }
            "new" => {
                self.start_new_chat();
            }
            "help" => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::Help;
            }
            _ => {
                self.state.status = format!("Unknown command: /{command}. Use /help");
            }
        }
    }

    fn ensure_selected_model(&mut self) {
        if let (Some(provider_id), Some(model_id)) = (
            self.state.selected_provider_id.as_ref(),
            self.state.selected_model_id.as_ref(),
        ) && self
            .state
            .models_by_provider
            .get(provider_id)
            .is_some_and(|models| models.iter().any(|model| &model.id == model_id))
        {
            return;
        }

        if let Some((provider_id, models)) = self.state.models_by_provider.iter().next()
            && let Some(model) = models.first()
        {
            self.state.selected_provider_id = Some(provider_id.clone());
            self.state.selected_model_id = Some(model.id.clone());
        }
    }

    fn apply_agent_profiles_state(&mut self, state: AgentProfilesState) {
        self.state.agent_profiles = state.profiles;
        self.state.agent_profile_warnings = state.warnings;
        self.state.selected_profile_id = state.selected_profile_id;
        self.state.profile_locked = state.profile_locked;
        if let Some(selected_profile_id) = self.state.selected_profile_id.as_ref()
            && let Some(index) = self
                .state
                .agent_profiles
                .iter()
                .position(|profile| &profile.id == selected_profile_id)
        {
            self.state.agent_menu_index = index;
        } else {
            self.state.agent_menu_index = 0;
        }
        if let Some(warning) = self.state.agent_profile_warnings.first() {
            self.state.status = format!("Agent profile warning: {}", warning.message);
        }
    }

    fn sync_current_session_profile_from_sessions(&mut self) {
        let Some(session_id) = self.state.current_session_id.as_ref() else {
            self.state.selected_profile_id = Some(String::from(DEFAULT_AGENT_PROFILE_ID));
            self.state.profile_locked = false;
            return;
        };
        if let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| &session.id == session_id)
        {
            self.state.selected_profile_id = session.selected_profile_id.clone();
            self.state.profile_locked = session.profile_locked;
        }
    }

    fn flatten_models(&self) -> Vec<(String, Model)> {
        flatten_models_map(&self.state.models_by_provider)
    }

    fn handle_providers_escape(&mut self) {
        if self.state.settings_editor.take().is_some() {
            self.state.settings_editor_input.clear();
            return;
        }

        match self.state.providers_view {
            ProvidersView::List => {
                self.state.mode = UiMode::Chat;
                self.state.status = String::from("Providers closed");
            }
            ProvidersView::Connect => {
                self.state.providers_view = ProvidersView::List;
                self.state.connect_provider_search.clear();
                self.state.connect_provider_index = 0;
                self.state.status = String::from("Provider connection cancelled");
            }
            ProvidersView::Detail => {
                self.state.providers_view = ProvidersView::List;
                self.state.status = String::from("Back to providers");
            }
            ProvidersView::Advanced => {
                self.state.providers_view = ProvidersView::Detail;
                self.state.settings_delete_armed = false;
                self.state.status = String::from("Back to provider detail");
            }
        }
    }

    fn handle_provider_list_key_event(&mut self, key_event: KeyEvent) {
        let len = self
            .state
            .settings_draft
            .as_ref()
            .map_or(0, |draft| draft.providers.len());

        match key_event.code {
            KeyCode::Up => {
                self.state.settings_provider_index =
                    adjust_index(self.state.settings_provider_index, len, -1);
                self.state.settings_model_index = 0;
                self.state.settings_delete_armed = false;
            }
            KeyCode::Down => {
                self.state.settings_provider_index =
                    adjust_index(self.state.settings_provider_index, len, 1);
                self.state.settings_model_index = 0;
                self.state.settings_delete_armed = false;
            }
            KeyCode::Enter => {
                if len == 0 {
                    self.state.status = String::from("No providers configured");
                    return;
                }
                self.state.providers_view = ProvidersView::Detail;
                self.maybe_request_openai_auth_status();
            }
            KeyCode::Char('a') => {
                self.state.providers_view = ProvidersView::Connect;
                self.state.connect_provider_search.clear();
                self.state.connect_provider_index = 0;
                self.state.status = String::from("Connect a provider");
            }
            KeyCode::Char('d') => self.delete_selected_provider_from_list(),
            KeyCode::Char('r') => {
                self.request(RuntimeRequest::ListModels);
                self.state.status = String::from("Refreshing provider models");
            }
            _ => {}
        }
    }

    fn handle_connect_provider_key_event(&mut self, key_event: KeyEvent) {
        let len = self.filtered_provider_definitions().len();
        match key_event.code {
            KeyCode::Up => {
                self.state.connect_provider_index =
                    adjust_index(self.state.connect_provider_index, len, -1);
            }
            KeyCode::Down => {
                self.state.connect_provider_index =
                    adjust_index(self.state.connect_provider_index, len, 1);
            }
            KeyCode::Backspace => {
                self.state.connect_provider_search.pop();
                self.state.connect_provider_index = 0;
            }
            KeyCode::Char(c) if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.state.connect_provider_search.push(c);
                self.state.connect_provider_index = 0;
            }
            KeyCode::Enter => self.create_provider_from_connect_selection(),
            _ => {}
        }
    }

    fn handle_provider_detail_key_event(&mut self, key_event: KeyEvent) {
        if matches!(key_event.code, KeyCode::Backspace) {
            self.state.providers_view = ProvidersView::List;
            self.state.status = String::from("Back to providers");
            return;
        }

        match key_event.code {
            KeyCode::Char('b') => {
                self.execute_provider_detail_action(ProviderDetailAction::BrowserLogin)
            }
            KeyCode::Char('c') => {
                self.execute_provider_detail_action(ProviderDetailAction::DeviceCodeLogin)
            }
            KeyCode::Char('x') => {
                self.execute_provider_detail_action(ProviderDetailAction::CancelLogin)
            }
            KeyCode::Char('l') => self.execute_provider_detail_action(ProviderDetailAction::Logout),
            KeyCode::Char('e') => {
                self.execute_provider_detail_action(ProviderDetailAction::Advanced)
            }
            KeyCode::Char('r') => {
                self.execute_provider_detail_action(ProviderDetailAction::RefreshModels)
            }
            KeyCode::Char('o') => self.retry_open_pending_auth_target(),
            KeyCode::Char('y') => self.copy_pending_auth_value(),
            _ => {}
        }
    }

    fn handle_advanced_provider_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Tab => self.cycle_advanced_focus(true),
            KeyCode::BackTab => self.cycle_advanced_focus(false),
            KeyCode::Up => self.move_advanced_selection(-1),
            KeyCode::Down => self.move_advanced_selection(1),
            KeyCode::Left => self.adjust_advanced_field(false),
            KeyCode::Right => self.adjust_advanced_field(true),
            KeyCode::Enter => self.activate_advanced_field(),
            KeyCode::Char('a') => self.add_advanced_item(),
            KeyCode::Char('d') => self.delete_advanced_item(),
            KeyCode::Char(' ') => self.toggle_settings_field(),
            KeyCode::Backspace => {
                self.state.providers_view = ProvidersView::Detail;
                self.state.status = String::from("Back to provider detail");
            }
            _ => {}
        }
    }

    fn cycle_advanced_focus(&mut self, forward: bool) {
        self.state.settings_delete_armed = false;
        self.state.providers_advanced_focus = match (self.state.providers_advanced_focus, forward) {
            (ProvidersAdvancedFocus::ProviderFields, true) => ProvidersAdvancedFocus::Models,
            (ProvidersAdvancedFocus::Models, true) => ProvidersAdvancedFocus::ModelFields,
            (ProvidersAdvancedFocus::ModelFields, true) => ProvidersAdvancedFocus::ProviderFields,
            (ProvidersAdvancedFocus::ProviderFields, false) => ProvidersAdvancedFocus::ModelFields,
            (ProvidersAdvancedFocus::Models, false) => ProvidersAdvancedFocus::ProviderFields,
            (ProvidersAdvancedFocus::ModelFields, false) => ProvidersAdvancedFocus::Models,
        };
        self.sync_settings_focus_from_advanced();
    }

    fn sync_settings_focus_from_advanced(&mut self) {
        self.state.settings_focus = match self.state.providers_advanced_focus {
            ProvidersAdvancedFocus::ProviderFields => SettingsFocus::ProviderForm,
            ProvidersAdvancedFocus::Models => SettingsFocus::ModelList,
            ProvidersAdvancedFocus::ModelFields => SettingsFocus::ModelForm,
        };
    }

    fn move_advanced_selection(&mut self, delta: isize) {
        self.sync_settings_focus_from_advanced();
        self.move_settings_selection(delta);
    }

    fn adjust_advanced_field(&mut self, forward: bool) {
        self.sync_settings_focus_from_advanced();
        self.adjust_settings_field(forward);
    }

    fn activate_advanced_field(&mut self) {
        self.sync_settings_focus_from_advanced();
        self.activate_settings_field();
    }

    fn add_advanced_item(&mut self) {
        self.sync_settings_focus_from_advanced();
        self.add_settings_item();
    }

    fn delete_advanced_item(&mut self) {
        self.sync_settings_focus_from_advanced();
        self.delete_settings_item();
    }

    fn delete_selected_provider_from_list(&mut self) {
        self.state.settings_focus = SettingsFocus::ProviderList;
        self.delete_settings_item();
    }

    fn filtered_provider_definitions(&self) -> Vec<&ProviderDefinition> {
        let query = self
            .state
            .connect_provider_search
            .trim()
            .to_ascii_lowercase();
        let mut definitions: Vec<&ProviderDefinition> = self
            .state
            .provider_definitions
            .iter()
            .filter(|definition| {
                query.is_empty()
                    || definition
                        .display_name
                        .to_ascii_lowercase()
                        .contains(&query)
                    || definition.type_id.to_ascii_lowercase().contains(&query)
                    || definition.description.to_ascii_lowercase().contains(&query)
            })
            .collect();
        definitions.sort_by(|left, right| {
            provider_definition_rank(left)
                .cmp(&provider_definition_rank(right))
                .then_with(|| left.display_name.cmp(&right.display_name))
                .then_with(|| left.type_id.cmp(&right.type_id))
        });
        definitions
    }

    fn create_provider_from_connect_selection(&mut self) {
        let definitions = self.filtered_provider_definitions();
        let Some((type_id, provider_fields, default_provider_id_prefix)) = definitions
            .get(self.state.connect_provider_index)
            .map(|definition| {
                (
                    definition.type_id.clone(),
                    definition.provider_fields.clone(),
                    definition.default_provider_id_prefix.clone(),
                )
            })
        else {
            self.state.status = String::from("No provider selected");
            return;
        };

        let Some(draft) = self.state.settings_draft.as_mut() else {
            self.state.status = String::from("Provider settings are not loaded yet");
            return;
        };

        let next_provider_id = next_provider_id(&draft.providers, &default_provider_id_prefix);
        draft.providers.push(ProviderSettings {
            id: next_provider_id.clone(),
            type_id,
            values: default_values(&provider_fields),
        });
        self.state.settings_provider_index = draft.providers.len().saturating_sub(1);
        self.state.settings_model_index = 0;
        self.state.settings_provider_field_index = 0;
        self.state.settings_model_field_index = 0;
        self.state.providers_view = ProvidersView::Detail;
        self.state.connect_provider_search.clear();
        self.state.connect_provider_index = 0;
        self.state.status = format!("Added provider: {next_provider_id}");
        self.save_settings_draft();
        self.maybe_request_openai_auth_status();
    }

    fn execute_provider_detail_action(&mut self, action: ProviderDetailAction) {
        match action {
            ProviderDetailAction::BrowserLogin => {
                if self
                    .current_provider()
                    .is_some_and(|provider| provider.type_id == "openai-codex")
                {
                    self.request(RuntimeRequest::StartOpenAiCodexBrowserLogin);
                    self.state.status = String::from("Starting OpenAI browser login");
                }
            }
            ProviderDetailAction::DeviceCodeLogin => {
                if self
                    .current_provider()
                    .is_some_and(|provider| provider.type_id == "openai-codex")
                {
                    self.request(RuntimeRequest::StartOpenAiCodexDeviceCodeLogin);
                    self.state.status = String::from("Starting OpenAI device-code login");
                }
            }
            ProviderDetailAction::CancelLogin => {
                if self
                    .current_provider()
                    .is_some_and(|provider| provider.type_id == "openai-codex")
                {
                    self.request(RuntimeRequest::CancelOpenAiCodexLogin);
                    self.state.status = String::from("Cancelling OpenAI login");
                }
            }
            ProviderDetailAction::Logout => {
                if self
                    .current_provider()
                    .is_some_and(|provider| provider.type_id == "openai-codex")
                {
                    self.request(RuntimeRequest::LogoutOpenAiCodexAuth);
                    self.state.status = String::from("Logging out from OpenAI");
                }
            }
            ProviderDetailAction::Advanced => {
                self.state.providers_view = ProvidersView::Advanced;
                self.state.providers_advanced_focus = ProvidersAdvancedFocus::ProviderFields;
                self.state.settings_focus = SettingsFocus::ProviderForm;
                self.state.status = String::from("Advanced provider config");
            }
            ProviderDetailAction::RefreshModels => {
                self.request(RuntimeRequest::ListModels);
                self.state.status = String::from("Refreshing provider models");
            }
        }
    }

    fn maybe_request_openai_auth_status(&self) {
        if self
            .current_provider()
            .is_some_and(|provider| provider.type_id == "openai-codex")
        {
            self.request(RuntimeRequest::GetOpenAiCodexAuthStatus);
        }
    }

    fn apply_openai_codex_auth_status(&mut self, status: ProviderAuthStatus) {
        let previous_target = pending_auth_target(&self.state.openai_codex_auth).map(str::to_owned);
        let next_target = pending_auth_target(&status).map(str::to_owned);
        let next_state = status.state;
        self.state.openai_codex_auth = status;

        if let Some(target) = next_target
            && previous_target.as_deref() != Some(target.as_str())
        {
            self.state.status = match open_external_target(&target) {
                Ok(()) => match next_state {
                    ProviderAuthState::BrowserPending => {
                        String::from("Opened browser for OpenAI sign-in. Press y to copy the URL.")
                    }
                    ProviderAuthState::DeviceCodePending => String::from(
                        "Opened browser for OpenAI device sign-in. Press y to copy the code.",
                    ),
                    ProviderAuthState::SignedOut | ProviderAuthState::Authenticated => {
                        String::from("Opened browser")
                    }
                },
                Err(err) => match next_state {
                    ProviderAuthState::BrowserPending => {
                        format!("Failed to open browser: {err}. Press y to copy the sign-in URL.")
                    }
                    ProviderAuthState::DeviceCodePending => {
                        format!("Failed to open browser: {err}. Press y to copy the device code.")
                    }
                    ProviderAuthState::SignedOut | ProviderAuthState::Authenticated => {
                        format!("Failed to open browser: {err}")
                    }
                },
            };
        }
    }

    fn retry_open_pending_auth_target(&mut self) {
        let target = pending_auth_target(&self.state.openai_codex_auth).map(str::to_owned);

        let Some(target) = target else {
            self.state.status = String::from("No pending auth URL available");
            return;
        };

        self.state.status = match open_external_target(&target) {
            Ok(()) => String::from("Opened browser"),
            Err(err) => format!("Failed to open browser: {err}"),
        };
    }

    fn copy_pending_auth_value(&mut self) {
        let value = match self.state.openai_codex_auth.state {
            ProviderAuthState::BrowserPending => self.state.openai_codex_auth.auth_url.clone(),
            ProviderAuthState::DeviceCodePending => self.state.openai_codex_auth.user_code.clone(),
            ProviderAuthState::SignedOut | ProviderAuthState::Authenticated => None,
        };

        let Some(value) = value else {
            self.state.status = String::from("Nothing to copy");
            return;
        };

        self.state.status = match self.copy_text_to_clipboard(&value) {
            Ok(()) => String::from("Copied to clipboard"),
            Err(err) => format!("Copy failed: {err}"),
        };
    }

    fn clear_chat_selection(&mut self) {
        self.state.selection = None;
    }

    fn hit_test_chat_cell(&self, column: u16, row: u16) -> Option<ChatCellPosition> {
        let view = self.state.visible_chat_view.as_ref()?;
        if column < view.area.x
            || row < view.area.y
            || column >= view.area.x.saturating_add(view.area.width)
            || row >= view.area.y.saturating_add(view.area.height)
        {
            return None;
        }

        let line_index = row.saturating_sub(view.area.y) as usize;
        let line = view.lines.get(line_index)?;
        let line_width = line.text.chars().count();
        if line_width == 0 {
            return Some(ChatCellPosition {
                line: line_index,
                column: 0,
            });
        }

        let local_x = column.saturating_sub(view.area.x) as usize;
        Some(ChatCellPosition {
            line: line_index,
            column: local_x.min(line_width.saturating_sub(1)),
        })
    }

    fn selected_chat_text(&self) -> Option<String> {
        let selection = self.state.selection?;
        let view = self.state.visible_chat_view.as_ref()?;
        selection_text(view, selection)
    }

    fn copy_selection_to_clipboard(&mut self) {
        let result = self.copy_selection_to_clipboard_inner();

        self.state.status = match result {
            Ok(true) => String::from("Copied selection to clipboard"),
            Ok(false) => String::from("No selection to copy"),
            Err(err) => format!("Copy failed: {err}"),
        };
    }

    fn copy_selection_to_clipboard_inner(&mut self) -> Result<bool, String> {
        let Some(text) = self.selected_chat_text() else {
            return Ok(false);
        };
        if text.is_empty() {
            return Ok(false);
        }

        let mut errors = Vec::new();
        let mut copied = false;

        match copy_via_osc52(&text) {
            Ok(()) => copied = true,
            Err(err) => errors.push(format!("terminal clipboard failed: {err}")),
        }

        match self.clipboard_mut() {
            Ok(clipboard) => match clipboard.set_text(text) {
                Ok(()) => copied = true,
                Err(err) => errors.push(format!("clipboard write failed: {err}")),
            },
            Err(err) => errors.push(err),
        }

        if copied {
            Ok(true)
        } else {
            Err(errors.join("; "))
        }
    }

    fn copy_text_to_clipboard(&mut self, text: &str) -> Result<(), String> {
        let mut errors = Vec::new();
        let mut copied = false;

        match copy_via_osc52(text) {
            Ok(()) => copied = true,
            Err(err) => errors.push(format!("terminal clipboard failed: {err}")),
        }

        match self.clipboard_mut() {
            Ok(clipboard) => match clipboard.set_text(text.to_string()) {
                Ok(()) => copied = true,
                Err(err) => errors.push(format!("clipboard write failed: {err}")),
            },
            Err(err) => errors.push(err),
        }

        if copied {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    #[cfg(test)]
    fn copy_selection_with<F>(&mut self, copy: F) -> Result<bool, String>
    where
        F: FnOnce(&str) -> Result<(), String>,
    {
        let Some(text) = self.selected_chat_text() else {
            return Ok(false);
        };
        if text.is_empty() {
            return Ok(false);
        }

        copy(&text)?;
        Ok(true)
    }

    fn clipboard_mut(&mut self) -> Result<&mut arboard::Clipboard, String> {
        if self.clipboard.is_none() {
            self.clipboard = Some(
                arboard::Clipboard::new().map_err(|err| format!("clipboard unavailable: {err}"))?,
            );
        }

        self.clipboard
            .as_mut()
            .ok_or_else(|| String::from("clipboard unavailable"))
    }

    fn insert_input_char(&mut self, ch: char) {
        let cursor = self.state.input_cursor.min(self.state.input.len());
        if self.state.input.is_char_boundary(cursor) {
            self.state.input.insert(cursor, ch);
            self.state.input_cursor = cursor + ch.len_utf8();
        }
    }

    fn insert_input_text(&mut self, text: &str) {
        let cursor = self.state.input_cursor.min(self.state.input.len());
        if self.state.input.is_char_boundary(cursor) {
            self.state.input.insert_str(cursor, text);
            self.state.input_cursor = cursor + text.len();
        }
    }

    fn backspace_input_char(&mut self) {
        let cursor = self.state.input_cursor.min(self.state.input.len());
        if cursor == 0 || !self.state.input.is_char_boundary(cursor) {
            return;
        }

        let prev = self
            .state
            .input
            .char_indices()
            .take_while(|(idx, _)| *idx < cursor)
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        self.state.input.drain(prev..cursor);
        self.state.input_cursor = prev;
    }

    fn move_input_cursor_left(&mut self) {
        let cursor = self.state.input_cursor.min(self.state.input.len());
        let prev = self
            .state
            .input
            .char_indices()
            .take_while(|(idx, _)| *idx < cursor)
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        self.state.input_cursor = prev;
    }

    fn move_input_cursor_right(&mut self) {
        let cursor = self.state.input_cursor.min(self.state.input.len());
        if cursor >= self.state.input.len() {
            self.state.input_cursor = self.state.input.len();
            return;
        }

        let next = self
            .state
            .input
            .char_indices()
            .map(|(idx, _)| idx)
            .find(|idx| *idx > cursor)
            .unwrap_or(self.state.input.len());
        self.state.input_cursor = next;
    }

    fn set_tool_approval(&mut self, call_id: &str, approved: Option<bool>) {
        if let Some(tool) = self
            .state
            .pending_tools
            .iter_mut()
            .find(|tool| tool.call_id == call_id)
        {
            tool.approved = approved;
        }
    }

    fn sort_pending_tools(&mut self) {
        self.state
            .pending_tools
            .sort_by_key(|tool| tool.queue_order);
    }

    fn current_pending_tool(&self) -> Option<&PendingTool> {
        self.state
            .pending_tools
            .iter()
            .find(|tool| tool.approved.is_none())
    }

    fn has_undecided_tools(&self) -> bool {
        self.state
            .pending_tools
            .iter()
            .any(|tool| tool.approved.is_none())
    }

    fn select_previous_tool_action(&mut self) {
        self.state.tool_approval_action = match self.state.tool_approval_action {
            ToolApprovalAction::Allow => ToolApprovalAction::Reject,
            ToolApprovalAction::Reject => ToolApprovalAction::Allow,
        };
    }

    fn select_next_tool_action(&mut self) {
        self.select_previous_tool_action();
    }

    fn confirm_current_tool_action(&mut self) {
        let approved = matches!(self.state.tool_approval_action, ToolApprovalAction::Allow);
        self.submit_tool_decision(approved);
    }

    fn submit_tool_decision(&mut self, approved: bool) {
        let Some(tool) = self.current_pending_tool() else {
            return;
        };
        let Some(session_id) = &self.state.current_session_id else {
            return;
        };

        if approved {
            self.request(RuntimeRequest::ApproveTool {
                session_id: session_id.clone(),
                call_id: tool.call_id.clone(),
            });
        } else {
            self.request(RuntimeRequest::DenyTool {
                session_id: session_id.clone(),
                call_id: tool.call_id.clone(),
            });
        }
    }

    fn enter_tool_decision_phase(&mut self) {
        self.clear_chat_selection();
        self.state.visible_chat_view = None;
        self.state.mode = UiMode::Chat;
        self.state.tool_phase = ToolPhase::Deciding;
        self.state.tool_approval_action = ToolApprovalAction::Allow;
    }

    fn sync_tool_phase_from_pending_tools(&mut self) {
        self.sort_pending_tools();
        if self.has_undecided_tools() {
            self.enter_tool_decision_phase();
            return;
        }

        if !self.state.pending_tools.is_empty() {
            self.state.mode = UiMode::Chat;
            self.state.tool_phase = ToolPhase::ExecutingBatch;
        } else if self.state.tool_phase != ToolPhase::ExecutingBatch {
            self.state.tool_phase = ToolPhase::Idle;
            self.state.tool_batch_execution_started = false;
        }
    }

    fn maybe_start_tool_batch_execution(&mut self) {
        if self.state.tool_phase != ToolPhase::ExecutingBatch
            || self.state.tool_batch_execution_started
            || self.state.pending_tools.is_empty()
            || self.has_undecided_tools()
        {
            return;
        }

        let Some(session_id) = &self.state.current_session_id else {
            return;
        };

        self.state.tool_batch_execution_started = true;
        self.state.status = format!(
            "Executing {} decided tool call(s)",
            self.state.pending_tools.len()
        );
        self.request(RuntimeRequest::ExecuteApprovedTools {
            session_id: session_id.clone(),
        });
    }

    fn finish_tool_batch_execution(&mut self) {
        self.state.tool_phase = ToolPhase::Idle;
        self.state.tool_batch_execution_started = false;
    }

    fn request_sync(&self) {
        self.request(RuntimeRequest::ListModels);
        self.request(RuntimeRequest::ListSessions);
        if let Some(session_id) = &self.state.current_session_id {
            self.request_sync_for_session(session_id);
        }
    }

    fn request_sync_for_session(&self, session_id: &str) {
        self.request(RuntimeRequest::GetCurrentTip {
            session_id: session_id.to_string(),
        });
        self.request(RuntimeRequest::GetChatHistory {
            session_id: session_id.to_string(),
        });
        self.request(RuntimeRequest::GetPendingTools {
            session_id: session_id.to_string(),
        });
        self.request(RuntimeRequest::ListAgentProfiles {
            session_id: session_id.to_string(),
        });
    }

    fn reset_chat_session(&mut self, session_id: Option<String>, status: &str) {
        let has_session = session_id.is_some();
        self.state.mode = UiMode::Chat;
        self.state.current_session_id = session_id;
        self.state.current_tip_id = None;
        self.state.chat_history.clear();
        self.state.optimistic_messages.clear();
        self.state.optimistic_tool_messages.clear();
        self.state.pending_tools.clear();
        self.state.agent_profiles = if has_session {
            Vec::new()
        } else {
            default_agent_profiles()
        };
        self.state.agent_profile_warnings.clear();
        self.state.selected_profile_id = if has_session {
            None
        } else {
            Some(String::from(DEFAULT_AGENT_PROFILE_ID))
        };
        self.state.profile_locked = false;
        self.state.tool_approval_action = ToolApprovalAction::Allow;
        self.state.tool_phase = ToolPhase::Idle;
        self.state.tool_batch_execution_started = false;
        self.state.is_streaming = false;
        self.state.auto_scroll = true;
        self.state.scroll = 0;
        self.state.status = status.to_string();
        self.invalidate_chat_cache();
        self.clamp_chat_scroll();
    }

    fn start_new_chat(&mut self) {
        self.state.pending_submit = None;
        self.reset_chat_session(None, "Started new chat");
    }

    fn dispatch_send_message(
        &mut self,
        session_id: String,
        message: String,
        model_id: String,
        provider_id: String,
        is_queued: bool,
    ) {
        let content_key = message.trim().to_string();
        let visible_count = self.visible_user_message_count(&content_key);
        let optimistic_same_count = self
            .state
            .optimistic_messages
            .iter()
            .filter(|optimistic| optimistic.content_key == content_key)
            .count();

        self.state.optimistic_seq = self.state.optimistic_seq.saturating_add(1);
        self.state.optimistic_messages.push(OptimisticMessage {
            local_id: format!("local-user-{}", self.state.optimistic_seq),
            content: message.clone(),
            content_key,
            occurrence: visible_count + optimistic_same_count + 1,
            is_queued,
        });

        if is_queued {
            self.update_queued_status();
        } else {
            self.state.is_streaming = true;
            self.state.status = format!("Sending with {provider_id}/{model_id}");
        }
        self.state.auto_scroll = true;
        self.state.current_tip_id = None;
        self.invalidate_chat_cache();

        self.request(RuntimeRequest::SendMessage {
            session_id,
            message,
            model_id,
            provider_id,
        });
    }

    fn request(&self, req: RuntimeRequest) {
        let _ = self.runtime_tx.send(req);
    }

    fn invalidate_chat_cache(&mut self) {
        self.state.chat_epoch = self.state.chat_epoch.wrapping_add(1);
        self.clear_chat_selection();
        self.state.visible_chat_view = None;
    }

    fn reconcile_optimistic_messages(&mut self) {
        if self.state.optimistic_messages.is_empty() {
            return;
        }

        let before_len = self.state.optimistic_messages.len();

        let visible_chain = build_tip_chain(
            &self.state.chat_history,
            self.state.current_tip_id.as_deref(),
        );
        let mut seen_users: HashMap<String, usize> = HashMap::new();
        for msg in visible_chain {
            if msg.role == ChatRole::User {
                let key = msg.content.trim().to_string();
                *seen_users.entry(key).or_insert(0) += 1;
            }
        }

        self.state.optimistic_messages.retain(|optimistic| {
            seen_users
                .get(&optimistic.content_key)
                .is_none_or(|count| *count < optimistic.occurrence)
        });

        if self.state.optimistic_messages.len() != before_len {
            self.update_queued_status();
            self.invalidate_chat_cache();
        }
    }

    fn visible_user_message_count(&self, content_key: &str) -> usize {
        build_tip_chain(&self.state.chat_history, self.state.current_tip_id.as_deref())
            .into_iter()
            .filter(|message| message.role == ChatRole::User)
            .filter(|message| message.content.trim() == content_key)
            .count()
    }

    fn update_queued_status(&mut self) {
        let queued_count = self
            .state
            .optimistic_messages
            .iter()
            .filter(|message| message.is_queued)
            .count();

        if queued_count > 0 {
            self.state.status = format!("Queued message ({queued_count} queued)");
        } else if self.state.status.starts_with("Queued message (") {
            self.state.status = String::from("Queued messages sent");
        }
    }

    fn reconcile_optimistic_tool_messages(&mut self) {
        if self.state.optimistic_tool_messages.is_empty() {
            return;
        }

        let before_len = self.state.optimistic_tool_messages.len();
        let visible_chain = build_tip_chain(
            &self.state.chat_history,
            self.state.current_tip_id.as_deref(),
        );
        let mut seen_tool_messages: HashMap<String, usize> = HashMap::new();
        for msg in visible_chain {
            if msg.role == ChatRole::Tool {
                *seen_tool_messages.entry(msg.content.clone()).or_insert(0) += 1;
            }
        }

        self.state.optimistic_tool_messages.retain(|optimistic| {
            match seen_tool_messages.get_mut(&optimistic.content) {
                Some(count) if *count > 0 => {
                    *count -= 1;
                    false
                }
                _ => true,
            }
        });

        if self.state.optimistic_tool_messages.len() != before_len {
            self.invalidate_chat_cache();
        }
    }

    fn push_optimistic_tool_message(
        &mut self,
        call_id: &str,
        tool_id: &str,
        output: &str,
        denied: bool,
    ) {
        if self
            .state
            .optimistic_tool_messages
            .iter()
            .any(|msg| msg.local_id == format!("local-tool-{call_id}"))
        {
            return;
        }

        let output_json = serde_json::from_str(output).unwrap_or_else(|_| {
            serde_json::json!({
                "error": "Failed to parse tool result",
                "raw_output": output,
            })
        });
        let content =
            types::format_tool_result_message(&types::ToolId::new(tool_id), &output_json, denied);

        self.state
            .optimistic_tool_messages
            .push(OptimisticToolMessage {
                local_id: format!("local-tool-{call_id}"),
                content,
            });
        self.invalidate_chat_cache();
    }
}

fn build_tip_chain<'a>(
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

fn flatten_models_map(models_by_provider: &HashMap<String, Vec<Model>>) -> Vec<(String, Model)> {
    let mut keys: Vec<&String> = models_by_provider.keys().collect();
    keys.sort();

    let mut flattened = Vec::new();
    for provider_id in keys {
        if let Some(models) = models_by_provider.get(provider_id) {
            for model in models {
                flattened.push((provider_id.clone(), model.clone()));
            }
        }
    }

    flattened
}

pub(super) fn provider_definition_rank(definition: &ProviderDefinition) -> (u8, String, String) {
    let display = definition.display_name.to_ascii_lowercase();
    let type_id = definition.type_id.to_ascii_lowercase();
    let rank = if display == "openai" || type_id == "openai-codex" {
        0
    } else if display.contains("openai") || type_id.contains("openai") {
        1
    } else {
        2
    };
    (rank, display, type_id)
}

fn next_provider_id(providers: &[ProviderSettings], prefix: &str) -> String {
    if !providers.iter().any(|provider| provider.id == prefix) {
        return prefix.to_string();
    }

    let mut next_index = 2usize;
    loop {
        let candidate = format!("{prefix}-{next_index}");
        if !providers.iter().any(|provider| provider.id == candidate) {
            return candidate;
        }
        next_index += 1;
    }
}

fn map_openai_codex_auth_status(status: RuntimeOpenAiCodexAuthStatus) -> ProviderAuthStatus {
    let mut mapped = ProviderAuthStatus {
        state: ProviderAuthState::SignedOut,
        email: status.email,
        plan_type: status.plan_type,
        account_id: status.account_id,
        last_refresh: status.last_refresh_unix.map(|value| value.to_string()),
        auth_url: None,
        verification_url: None,
        user_code: None,
        error: status.error,
    };

    mapped.state = match status.state {
        RuntimeOpenAiCodexLoginState::SignedOut => ProviderAuthState::SignedOut,
        RuntimeOpenAiCodexLoginState::BrowserPending(pending) => {
            mapped.auth_url = Some(pending.auth_url);
            ProviderAuthState::BrowserPending
        }
        RuntimeOpenAiCodexLoginState::DeviceCodePending(pending) => {
            mapped.verification_url = Some(pending.verification_url);
            mapped.user_code = Some(pending.user_code);
            ProviderAuthState::DeviceCodePending
        }
        RuntimeOpenAiCodexLoginState::Authenticated => ProviderAuthState::Authenticated,
    };

    mapped
}

fn pending_auth_target(status: &ProviderAuthStatus) -> Option<&str> {
    match status.state {
        ProviderAuthState::BrowserPending => status.auth_url.as_deref(),
        ProviderAuthState::DeviceCodePending => status.verification_url.as_deref(),
        ProviderAuthState::SignedOut | ProviderAuthState::Authenticated => None,
    }
}

fn open_external_target(target: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let command = ("open", vec![target]);
    #[cfg(target_os = "linux")]
    let command = ("xdg-open", vec![target]);
    #[cfg(target_os = "windows")]
    let command = ("cmd", vec!["/C", "start", "", target]);

    std::process::Command::new(command.0)
        .args(command.1)
        .spawn()
        .map_err(|err| err.to_string())
        .map(|_| ())
}

fn message_fingerprint(msg: &Message) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    msg.id.as_str().hash(&mut hasher);
    msg.parent_id
        .as_ref()
        .map(|id| id.as_str())
        .hash(&mut hasher);
    match msg.role {
        ChatRole::System => 0u8,
        ChatRole::User => 1u8,
        ChatRole::Assistant => 2u8,
        ChatRole::Tool => 3u8,
    }
    .hash(&mut hasher);
    match &msg.status {
        MessageStatus::Complete => 0u8.hash(&mut hasher),
        MessageStatus::Streaming { call_id } => {
            1u8.hash(&mut hasher);
            call_id.as_str().hash(&mut hasher);
        }
        MessageStatus::ProcessingTools => 2u8.hash(&mut hasher),
        MessageStatus::Cancelled => 3u8.hash(&mut hasher),
    }
    msg.content.hash(&mut hasher);
    hasher.finish()
}

impl AppState {
    fn chat_max_scroll(&self) -> u16 {
        let cache = self.chat_render_cache.borrow();
        cache.total_lines.saturating_sub(self.chat_viewport_height)
    }

    fn rendered_messages(&self) -> Vec<Message> {
        let mut rendered_messages: Vec<Message> =
            build_tip_chain(&self.chat_history, self.current_tip_id.as_deref())
                .into_iter()
                .cloned()
                .collect();

        for optimistic in &self.optimistic_messages {
            let content = if optimistic.is_queued {
                format!("{} [queued]", optimistic.content)
            } else {
                optimistic.content.clone()
            };
            rendered_messages.push(Message {
                id: MessageId::new(optimistic.local_id.clone()),
                parent_id: None,
                role: ChatRole::User,
                content,
                status: MessageStatus::Complete,
                agent_profile_id: self.selected_profile_id.clone(),
                tool_state_snapshot: None,
                tool_state_deltas: Vec::new(),
            });
        }

        for optimistic in &self.optimistic_tool_messages {
            rendered_messages.push(Message {
                id: MessageId::new(optimistic.local_id.clone()),
                parent_id: None,
                role: ChatRole::Tool,
                content: optimistic.content.clone(),
                status: MessageStatus::Complete,
                agent_profile_id: self.selected_profile_id.clone(),
                tool_state_snapshot: None,
                tool_state_deltas: Vec::new(),
            });
        }

        rendered_messages
    }
}

fn is_ctrl_c(key_event: KeyEvent) -> bool {
    key_event.code == KeyCode::Char('c') && key_event.modifiers == KeyModifiers::CONTROL
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use agent_runtime::{
        AgentProfileSummary, Event, FieldDefinition, FieldValueEntry, FieldValueKind, Model,
        ModelSettings, ProviderDefinition, ProviderSettings, Session, SettingsDocument,
        SettingsValue,
    };
    use crossbeam_channel::{Receiver, unbounded};
    use ratatui::{
        buffer::Buffer,
        crossterm::event::{
            Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent,
            MouseEventKind,
        },
        layout::Rect,
        style::{Color, Style},
        widgets::Widget,
    };
    use types::{ChatRole, Message, MessageId, MessageStatus};

    use super::{
        ActiveSettingsEditor, App, AppState, ChatCellPosition, ChatSelection, OptimisticMessage,
        OptimisticToolMessage, PendingSubmit, PendingTool, ProviderAuthState, ProviderAuthStatus,
        ProvidersAdvancedFocus, ProvidersView, RuntimeRequest, RuntimeResponse, SettingsModelField,
        SettingsProviderField, ToolPhase, UiMode, default_agent_profiles, is_copy_shortcut,
        menu_scroll_offset, model_menu_next_index, model_menu_previous_index,
        render_chat_selection_overlay, selection_text,
    };
    use crate::components::VisibleChatView;

    struct TestHarness {
        app: App,
        requests_rx: Receiver<RuntimeRequest>,
    }

    fn test_harness() -> TestHarness {
        let (_event_tx, event_rx) = unbounded();
        let (runtime_tx, requests_rx) = unbounded();
        let (_response_tx, runtime_rx) = unbounded();

        TestHarness {
            app: App {
                event_rx,
                runtime_tx,
                runtime_rx,
                clipboard: None,
                state: AppState::default(),
                last_stream_refresh: None,
            },
            requests_rx,
        }
    }

    impl TestHarness {
        fn drain_requests(&self) -> Vec<RuntimeRequest> {
            let mut requests = Vec::new();
            while let Ok(request) = self.requests_rx.try_recv() {
                requests.push(request);
            }
            requests
        }

        fn set_chat_metrics(&mut self, total_lines: u16, viewport_height: u16) {
            let mut cache = self.app.state.chat_render_cache.borrow_mut();
            cache.total_lines = total_lines;
            drop(cache);
            self.app.state.chat_viewport_height = viewport_height;
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn ctrl_shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL | KeyModifiers::SHIFT)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn message(id: &str, parent_id: Option<&str>, role: ChatRole, content: &str) -> Message {
        Message {
            id: MessageId::new(id),
            parent_id: parent_id.map(MessageId::new),
            role,
            content: content.to_string(),
            status: MessageStatus::Complete,
            agent_profile_id: None,
            tool_state_snapshot: None,
            tool_state_deltas: Vec::new(),
        }
    }

    fn sample_models() -> HashMap<String, Vec<Model>> {
        HashMap::from([(
            String::from("openai-chat-completions"),
            vec![
                Model {
                    id: String::from("gpt-4.1-mini"),
                    name: String::from("GPT-4.1 Mini"),
                },
                Model {
                    id: String::from("gpt-4o-mini"),
                    name: String::from("GPT-4o Mini"),
                },
            ],
        )])
    }

    fn sample_settings() -> SettingsDocument {
        SettingsDocument {
            providers: vec![ProviderSettings {
                id: String::from("openai-chat-completions"),
                type_id: String::from("openai-chat-completions"),
                values: vec![
                    FieldValueEntry {
                        key: String::from("base_url"),
                        value: SettingsValue::String(String::from("https://api.openai.com/v1")),
                    },
                    FieldValueEntry {
                        key: String::from("env_var_api_key"),
                        value: SettingsValue::String(String::from("OPENAI_API_KEY")),
                    },
                    FieldValueEntry {
                        key: String::from("only_listed_models"),
                        value: SettingsValue::Bool(true),
                    },
                ],
            }],
            models: vec![ModelSettings {
                id: String::from("gpt-4o-mini"),
                provider_id: String::from("openai-chat-completions"),
                values: vec![
                    FieldValueEntry {
                        key: String::from("name"),
                        value: SettingsValue::String(String::from("GPT-4o Mini")),
                    },
                    FieldValueEntry {
                        key: String::from("max_context"),
                        value: SettingsValue::Integer(128_000),
                    },
                ],
            }],
        }
    }

    fn sample_provider_definitions() -> Vec<ProviderDefinition> {
        vec![
            ProviderDefinition {
                type_id: String::from("openai-codex"),
                display_name: String::from("OpenAI Codex"),
                protocol_family: String::from("openai-responses"),
                description: String::from("ChatGPT subscription auth"),
                provider_fields: vec![],
                model_fields: vec![
                    FieldDefinition {
                        key: String::from("name"),
                        label: String::from("Display Name"),
                        value_kind: FieldValueKind::String,
                        required: false,
                        secret: false,
                        help_text: None,
                        default_value: None,
                    },
                    FieldDefinition {
                        key: String::from("max_context"),
                        label: String::from("Max Context"),
                        value_kind: FieldValueKind::Integer,
                        required: false,
                        secret: false,
                        help_text: None,
                        default_value: None,
                    },
                ],
                supports_model_discovery: true,
                default_provider_id_prefix: String::from("openai-codex"),
            },
            ProviderDefinition {
                type_id: String::from("openai-chat-completions"),
                display_name: String::from("OpenAI-compatible Chat Completions"),
                protocol_family: String::from("openai-chat-completions"),
                description: String::from("Test definition"),
                provider_fields: vec![
                    FieldDefinition {
                        key: String::from("base_url"),
                        label: String::from("Base URL"),
                        value_kind: FieldValueKind::Url,
                        required: true,
                        secret: false,
                        help_text: None,
                        default_value: Some(SettingsValue::String(String::from(
                            "https://api.openai.com/v1",
                        ))),
                    },
                    FieldDefinition {
                        key: String::from("api_key"),
                        label: String::from("Inline API Key"),
                        value_kind: FieldValueKind::String,
                        required: false,
                        secret: true,
                        help_text: None,
                        default_value: None,
                    },
                    FieldDefinition {
                        key: String::from("env_var_api_key"),
                        label: String::from("Env Var"),
                        value_kind: FieldValueKind::String,
                        required: false,
                        secret: false,
                        help_text: None,
                        default_value: Some(SettingsValue::String(String::from("OPENAI_API_KEY"))),
                    },
                    FieldDefinition {
                        key: String::from("only_listed_models"),
                        label: String::from("Only Listed Models"),
                        value_kind: FieldValueKind::Boolean,
                        required: false,
                        secret: false,
                        help_text: None,
                        default_value: Some(SettingsValue::Bool(true)),
                    },
                ],
                model_fields: vec![
                    FieldDefinition {
                        key: String::from("name"),
                        label: String::from("Display Name"),
                        value_kind: FieldValueKind::String,
                        required: false,
                        secret: false,
                        help_text: None,
                        default_value: None,
                    },
                    FieldDefinition {
                        key: String::from("max_context"),
                        label: String::from("Max Context"),
                        value_kind: FieldValueKind::Integer,
                        required: false,
                        secret: false,
                        help_text: None,
                        default_value: None,
                    },
                ],
                supports_model_discovery: true,
                default_provider_id_prefix: String::from("openai-chat-completions"),
            },
        ]
    }

    fn sample_sessions() -> Vec<Session> {
        vec![
            Session {
                id: String::from("sess-1"),
                tip_id: Some(String::from("m2")),
                workspace_dir: String::from("/tmp/project-a"),
                created_at: 1,
                updated_at: 2,
                title: Some(String::from("Refactor ideas")),
                selected_profile_id: Some(String::from("plan-code")),
                profile_locked: true,
                waiting_for_approval: true,
                is_streaming: true,
            },
            Session {
                id: String::from("sess-2"),
                tip_id: Some(String::from("m3")),
                workspace_dir: String::from("/tmp/project-b"),
                created_at: 3,
                updated_at: 4,
                title: Some(String::from("Testing plan")),
                selected_profile_id: Some(String::from("build-code")),
                profile_locked: false,
                waiting_for_approval: false,
                is_streaming: false,
            },
        ]
    }

    fn sample_agent_profiles() -> Vec<AgentProfileSummary> {
        default_agent_profiles()
    }

    fn sample_pending_tools() -> Vec<PendingTool> {
        vec![
            PendingTool {
                call_id: String::from("call-1"),
                tool_id: String::from("read_file"),
                args: String::from("{\"path\":\"src/app.rs\"}"),
                description: String::from("Inspect the app module"),
                risk_level: String::from("read_only_workspace"),
                reasons: vec![
                    String::from("Reads local source files"),
                    String::from("Needed to answer the user"),
                ],
                approved: Some(true),
                queue_order: 0,
            },
            PendingTool {
                call_id: String::from("call-2"),
                tool_id: String::from("write_file"),
                args: String::from("{\"path\":\"src/app.rs\",\"content\":\"...\"}"),
                description: String::from("Patch the app module"),
                risk_level: String::from("undoable_workspace_write"),
                reasons: vec![String::from("Updates tracked source code")],
                approved: None,
                queue_order: 1,
            },
        ]
    }

    fn populated_state() -> AppState {
        AppState {
            config_loaded: true,
            status: String::from("Ready"),
            models_by_provider: sample_models(),
            agent_profiles: sample_agent_profiles(),
            provider_definitions: sample_provider_definitions(),
            selected_profile_id: Some(String::from("plan-code")),
            selected_provider_id: Some(String::from("openai-chat-completions")),
            selected_model_id: Some(String::from("gpt-4o-mini")),
            pending_tools: sample_pending_tools(),
            sessions: sample_sessions(),
            current_session_id: Some(String::from("sess-2")),
            current_tip_id: Some(String::from("m2")),
            settings_draft: Some(sample_settings()),
            chat_history: BTreeMap::from([
                (
                    MessageId::new("m1"),
                    message("m1", None, ChatRole::User, "How should we test the TUI?"),
                ),
                (
                    MessageId::new("m2"),
                    message(
                        "m2",
                        Some("m1"),
                        ChatRole::Assistant,
                        "Use render tests, interaction tests, and a small number of end-to-end smoke tests.",
                    ),
                ),
            ]),
            ..AppState::default()
        }
    }

    fn render_state_snapshot(state: &AppState, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        state.render(area, &mut buffer);
        buffer_to_snapshot(&buffer)
    }

    fn buffer_to_snapshot(buffer: &Buffer) -> String {
        let mut lines = Vec::new();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            let trimmed = line.trim_end();
            if !trimmed.is_empty() {
                lines.push(format!("{y:02}: {trimmed}"));
            }
        }
        lines.join("\n")
    }

    fn visible_chat_view(lines: &[&str], area: Rect) -> VisibleChatView {
        VisibleChatView::from_strings(area, lines)
    }

    fn assert_snapshot(actual: &str, expected: &str) {
        if actual != expected {
            panic!("snapshot mismatch\n--- actual ---\n{actual}\n--- expected ---\n{expected}");
        }
    }

    #[test]
    fn model_menu_scroll_stays_at_top_when_selection_is_visible() {
        assert_eq!(menu_scroll_offset(3, 20, 8), 0);
    }

    #[test]
    fn model_menu_scroll_follows_selection_past_bottom() {
        assert_eq!(menu_scroll_offset(9, 20, 8), 2);
    }

    #[test]
    fn model_menu_scroll_clamps_to_max_scroll() {
        assert_eq!(menu_scroll_offset(19, 20, 8), 12);
    }

    #[test]
    fn menu_scroll_with_zero_visible_lines_stays_at_top() {
        assert_eq!(menu_scroll_offset(10, 20, 0), 0);
    }

    #[test]
    fn menu_scroll_when_content_fits_stays_at_top() {
        assert_eq!(menu_scroll_offset(4, 5, 8), 0);
    }

    #[test]
    fn page_down_clamps_stored_scroll_to_chat_bottom() {
        let mut harness = test_harness();
        harness.set_chat_metrics(20, 8);
        harness.app.state.auto_scroll = false;
        harness.app.state.scroll = 11;

        harness.app.handle_chat_key_event(key(KeyCode::PageDown));

        assert_eq!(harness.app.state.scroll, 12);
        assert!(!harness.app.state.auto_scroll);
    }

    #[test]
    fn mouse_scroll_down_does_not_accumulate_hidden_overscroll() {
        let mut harness = test_harness();
        harness.set_chat_metrics(20, 8);
        harness.app.state.auto_scroll = false;
        harness.app.state.scroll = 12;

        harness.app.handle_mouse_event(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(harness.app.state.scroll, 12);
    }

    #[test]
    fn page_up_saturates_at_zero() {
        let mut harness = test_harness();
        harness.set_chat_metrics(20, 8);
        harness.app.state.auto_scroll = false;
        harness.app.state.scroll = 4;

        harness.app.handle_chat_key_event(key(KeyCode::PageUp));

        assert_eq!(harness.app.state.scroll, 0);
        assert!(!harness.app.state.auto_scroll);
    }

    #[test]
    fn clamp_chat_scroll_reduces_offset_when_viewport_grows() {
        let mut harness = test_harness();
        harness.set_chat_metrics(20, 8);
        harness.app.state.auto_scroll = false;
        harness.app.state.scroll = 12;

        harness.app.update_chat_viewport(12);

        assert_eq!(harness.app.state.scroll, 8);
    }

    #[test]
    fn end_reenables_auto_scroll() {
        let mut harness = test_harness();
        harness.set_chat_metrics(20, 8);
        harness.app.state.auto_scroll = false;
        harness.app.state.scroll = 5;

        harness.app.handle_chat_key_event(key(KeyCode::End));

        assert!(harness.app.state.auto_scroll);
        assert_eq!(harness.app.state.scroll, 5);
    }

    #[test]
    fn model_menu_next_index_wraps_at_end() {
        assert_eq!(model_menu_next_index(4, 5), 0);
    }

    #[test]
    fn model_menu_next_index_advances_within_bounds() {
        assert_eq!(model_menu_next_index(2, 5), 3);
    }

    #[test]
    fn model_menu_previous_index_wraps_at_start() {
        assert_eq!(model_menu_previous_index(0, 5), 4);
    }

    #[test]
    fn model_menu_previous_index_moves_back_within_bounds() {
        assert_eq!(model_menu_previous_index(3, 5), 2);
    }

    #[test]
    fn renders_chat_screen_snapshot() {
        let mut state = populated_state();
        state.input = String::from("Add tests for the settings menu");
        state.input_cursor = state.input.len();

        let rendered = render_state_snapshot(&state, 72, 18);

        assert_snapshot(
            &rendered,
            r#"01: How should we test the TUI?
04: Use render tests, interaction tests, and a small number of end-to-end sm
05: oke tests.
14: Ready | agent=plan-code | model=openai-chat-completions/gpt-4o-mini | to
16:  > Add tests for the settings menu"#,
        );
    }

    #[test]
    fn renders_command_popup_snapshot() {
        let mut state = populated_state();
        state.input = String::from("/s");
        state.input_cursor = state.input.len();

        let rendered = render_state_snapshot(&state, 72, 18);

        assert_snapshot(
            &rendered,
            r#"01: How should we test the TUI?
04: Use render tests, interaction tests, and a small number of end-to-end sm
05: oke tests.
12:  ┌Command (Tab/Down next, Shift-Tab/Up prev┐
13:  │> /sessions  Open sessions menu          │
14: R└─────────────────────────────────────────┘completions/gpt-4o-mini | to
16:  > /s"#,
        );
    }

    #[test]
    fn renders_model_menu_snapshot() {
        let mut state = populated_state();
        state.mode = UiMode::ModelMenu;
        state.model_menu_index = 1;

        let rendered = render_state_snapshot(&state, 72, 18);

        assert_snapshot(
            &rendered,
            r#"01: How should we test the TUI?
04: Use rende┌/model──────────────────────────────────────────────┐to-end sm
05: oke tests│Select model (Enter to choose, Esc to close)        │
06:          │  openai-chat-completions / GPT-4.1 Mini            │
07:          │> openai-chat-completions / GPT-4o Mini (current)   │
08:          │                                                    │
09:          │                                                    │
10:          │                                                    │
11:          │                                                    │
12:          └────────────────────────────────────────────────────┘
14: Ready | agent=plan-code | model=openai-chat-completions/gpt-4o-mini | to
16:  >"#,
        );
    }

    #[test]
    fn renders_providers_list_snapshot() {
        let mut state = populated_state();
        state.mode = UiMode::ProvidersMenu;
        state.providers_view = ProvidersView::List;

        let rendered = render_state_snapshot(&state, 100, 22);

        assert_snapshot(
            &rendered,
            r#"01: How should we test the TUI?
02:     ┌/providers───────────────────────────────────────────────────────────────────────────────┐
03:     │Providers                                                                                │
04: Use │Enter=open, a=connect, d=delete, r=refresh, Esc=close                                    │
05:     │┌───────────────────────────────────────────────────────────────────────────────────────┐│
06:     ││Configured providers                                                                   ││
07:     ││> id=openai-chat-completions  OpenAI-compatible Chat Completions                       ││
08:     ││type=openai-chat-completions  models=2                                                 ││
09:     ││                                                                                       ││
10:     ││                                                                                       ││
11:     ││                                                                                       ││
12:     ││                                                                                       ││
13:     ││                                                                                       ││
14:     │└───────────────────────────────────────────────────────────────────────────────────────┘│
15:     │┌───────────────────────────────────────────────────────────────────────────────────────┐│
16:     ││One provider panel at a time                                                           ││
17:     │└───────────────────────────────────────────────────────────────────────────────────────┘│
18: Read└─────────────────────────────────────────────────────────────────────────────────────────┘
20:  >"#,
        );
    }

    #[test]
    fn provider_detail_keeps_openai_actions_and_id_visible() {
        let mut state = populated_state();
        state.mode = UiMode::ProvidersMenu;
        state.providers_view = ProvidersView::Detail;
        state.settings_provider_index = 1;
        state.settings_draft = Some(SettingsDocument {
            providers: vec![
                ProviderSettings {
                    id: String::from("openai-chat-completions"),
                    type_id: String::from("openai-chat-completions"),
                    values: vec![],
                },
                ProviderSettings {
                    id: String::from("openai"),
                    type_id: String::from("openai-codex"),
                    values: vec![],
                },
            ],
            models: vec![],
        });

        let rendered = render_state_snapshot(&state, 100, 18);

        assert!(rendered.contains("Provider: id=openai"));
        assert!(rendered.contains("State: Signed out"));
        assert!(rendered.contains("Actions"));
        assert!(rendered.contains("b browser sign-in"));
    }

    #[test]
    fn provider_detail_browser_pending_hides_raw_auth_url() {
        let mut state = populated_state();
        state.mode = UiMode::ProvidersMenu;
        state.providers_view = ProvidersView::Detail;
        state.settings_provider_index = 1;
        state.settings_draft = Some(SettingsDocument {
            providers: vec![
                ProviderSettings {
                    id: String::from("openai-chat-completions"),
                    type_id: String::from("openai-chat-completions"),
                    values: vec![],
                },
                ProviderSettings {
                    id: String::from("openai"),
                    type_id: String::from("openai-codex"),
                    values: vec![],
                },
            ],
            models: vec![],
        });
        state.openai_codex_auth = ProviderAuthStatus {
            state: ProviderAuthState::BrowserPending,
            auth_url: Some(String::from(
                "https://auth.openai.com/oauth/authorize?example=1",
            )),
            ..ProviderAuthStatus::default()
        };

        let rendered = render_state_snapshot(&state, 120, 22);

        assert!(rendered.contains("Browser should open automatically."));
        assert!(rendered.contains("y copy sign-in URL  o open again  x cancel"));
        assert!(!rendered.contains("https://auth.openai.com/oauth/authorize"));
    }

    #[test]
    fn renders_connect_provider_snapshot() {
        let mut state = populated_state();
        state.mode = UiMode::ProvidersMenu;
        state.providers_view = ProvidersView::Connect;

        let rendered = render_state_snapshot(&state, 100, 22);

        assert!(rendered.contains("/providers"));
        assert!(rendered.contains("Connect a provider"));
        assert!(rendered.contains("OpenAI"));
    }

    #[test]
    fn renders_sessions_menu_snapshot() {
        let mut state = populated_state();
        state.mode = UiMode::SessionsMenu;
        state.sessions_menu_index = 2;

        let rendered = render_state_snapshot(&state, 72, 18);

        assert_snapshot(
            &rendered,
            r#"01: How should we test the TUI?
04: Use ren┌/sessions──────────────────────────────────────────────┐o-end sm
05: oke tes│Sessions (Enter=load/new, x=delete, Esc=close)         │
06:        │  Start new chat                                       │
07:        │  Refactor ideas [approval] [streaming]                │
08:        │> Testing plan (current)                               │
09:        │                                                       │
10:        │                                                       │
11:        │                                                       │
12:        └───────────────────────────────────────────────────────┘
14: Ready | agent=plan-code | model=openai-chat-completions/gpt-4o-mini | to
16:  >"#,
        );
    }

    #[test]
    fn renders_tool_approval_panel_snapshot() {
        let mut state = populated_state();
        state.tool_phase = ToolPhase::Deciding;

        let rendered = render_state_snapshot(&state, 80, 20);

        assert_snapshot(
            &rendered,
            r#"01: How should we test the TUI?
04: Use render tests, interaction tests, and a small number of end-to-end smoke test
05: s.
09: Ready | agent=plan-code | model=openai-chat-completions/gpt-4o-mini | tools=2 |
10:   Permission required ─────────────────────────────────────────────────────────┐
11:   Patch the app module                                                         │
12:   tool: write_file  risk: undoable_workspace_write                             │
13:   why: Updates tracked source code                                             │
14:                                                                                │
15:   args                                                                         │
16:   {"path":"src/app.rs","content":"..."}                                        │
17:                                                                                │
18:    Allow   Reject                                           select <->  confir │
19:  ──────────────────────────────────────────────────────────────────────────────┘"#,
        );
    }

    #[test]
    fn renders_help_menu_snapshot() {
        let mut state = populated_state();
        state.mode = UiMode::Help;

        let rendered = render_state_snapshot(&state, 72, 18);

        assert_snapshot(
            &rendered,
            r#"01: How should we test the TUI?
04: Use render tes┌/help────────────────────────────────────┐f end-to-end sm
05: oke tests.    │Slash Commands                           │
06:               │/agent     Open agent selector           │
07:               │/help      Open this help menu           │
08:               │/model     Open model selector           │
09:               │/new       Start a new chat              │
10:               │/providers Open providers                │
11:               │/sessions  Open sessions menu            │
12:               └─────────────────────────────────────────┘
14: Ready | agent=plan-code | model=openai-chat-completions/gpt-4o-mini | to
16:  >"#,
        );
    }

    #[test]
    fn selection_text_preserves_visible_line_breaks() {
        let view = visible_chat_view(&["alpha", "", "beta"], Rect::new(0, 0, 10, 3));
        let selection = ChatSelection {
            anchor: ChatCellPosition { line: 0, column: 2 },
            focus: ChatCellPosition { line: 2, column: 2 },
        };

        assert_eq!(
            selection_text(&view, selection),
            Some(String::from("pha\n\nbet"))
        );
    }

    #[test]
    fn copy_shortcut_requires_control_and_shift() {
        assert!(is_copy_shortcut(ctrl_shift_key(KeyCode::Char('c'))));
        assert!(!is_copy_shortcut(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn copy_selection_uses_rendered_chat_text() {
        let mut harness = test_harness();
        harness.app.state.visible_chat_view = Some(visible_chat_view(
            &["alpha beta", "gamma"],
            Rect::new(0, 0, 12, 2),
        ));
        harness.app.state.selection = Some(ChatSelection {
            anchor: ChatCellPosition { line: 0, column: 6 },
            focus: ChatCellPosition { line: 1, column: 2 },
        });

        let mut copied = None;
        let result = harness.app.copy_selection_with(|text| {
            copied = Some(text.to_string());
            Ok(())
        });

        assert_eq!(result, Ok(true));
        assert_eq!(copied, Some(String::from("beta\ngam")));
    }

    #[test]
    fn first_ctrl_c_clears_chat_input_without_exiting() {
        let mut harness = test_harness();
        harness.app.state.input = String::from("/s");
        harness.app.state.input_cursor = harness.app.state.input.len();
        harness.app.state.command_popup_dismissed = true;
        harness.app.state.command_completion_prefix = Some(String::from("s"));
        harness.app.state.command_completion_index = 1;
        harness.app.state.selection = Some(ChatSelection {
            anchor: ChatCellPosition { line: 0, column: 0 },
            focus: ChatCellPosition { line: 0, column: 1 },
        });
        harness.app.state.visible_chat_view =
            Some(visible_chat_view(&["alpha"], Rect::new(0, 0, 10, 1)));

        harness.app.handle_key_event(ctrl_key(KeyCode::Char('c')));

        assert!(!harness.app.state.exit);
        assert!(harness.app.state.ctrl_c_exit_armed);
        assert!(harness.app.state.input.is_empty());
        assert_eq!(harness.app.state.input_cursor, 0);
        assert_eq!(harness.app.state.selection, None);
        assert_eq!(harness.app.state.visible_chat_view, None);
        assert!(!harness.app.state.command_popup_dismissed);
        assert_eq!(harness.app.state.command_completion_prefix, None);
        assert_eq!(harness.app.state.command_completion_index, 0);
        assert_eq!(
            harness.app.state.status,
            "Cleared input. Press Ctrl+C again to exit"
        );
        assert!(harness.drain_requests().is_empty());
    }

    #[test]
    fn second_consecutive_ctrl_c_exits_chat() {
        let mut harness = test_harness();

        harness.app.handle_key_event(ctrl_key(KeyCode::Char('c')));
        harness.app.handle_key_event(ctrl_key(KeyCode::Char('c')));

        assert!(harness.app.state.exit);
        assert!(harness.drain_requests().is_empty());
    }

    #[test]
    fn non_ctrl_c_key_disarms_ctrl_c_exit_confirmation() {
        let mut harness = test_harness();
        harness.app.state.input = String::from("hello");
        harness.app.state.input_cursor = harness.app.state.input.len();

        harness.app.handle_key_event(ctrl_key(KeyCode::Char('c')));
        assert!(harness.app.state.ctrl_c_exit_armed);

        harness.app.handle_key_event(key(KeyCode::Char('x')));
        assert!(!harness.app.state.ctrl_c_exit_armed);
        assert_eq!(harness.app.state.input, "x");

        harness.app.handle_key_event(ctrl_key(KeyCode::Char('c')));
        assert!(!harness.app.state.exit);
        assert!(harness.app.state.ctrl_c_exit_armed);
        assert!(harness.app.state.input.is_empty());
    }

    #[test]
    fn ctrl_c_in_non_chat_mode_preserves_existing_behavior() {
        let mut harness = test_harness();
        harness.app.state.mode = UiMode::Help;

        harness.app.handle_key_event(ctrl_key(KeyCode::Char('c')));

        assert_eq!(harness.app.state.mode, UiMode::Help);
        assert!(!harness.app.state.exit);
        assert!(!harness.app.state.ctrl_c_exit_armed);
    }

    #[test]
    fn mouse_drag_updates_selection_in_chat_view() {
        let mut harness = test_harness();
        harness.app.state.visible_chat_view = Some(visible_chat_view(
            &["alpha", "beta"],
            Rect::new(0, 0, 10, 2),
        ));

        harness.app.handle_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        harness.app.handle_mouse_event(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 2,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(
            harness.app.state.selection,
            Some(ChatSelection {
                anchor: ChatCellPosition { line: 0, column: 1 },
                focus: ChatCellPosition { line: 1, column: 2 },
            })
        );
    }

    #[test]
    fn selection_overlay_marks_buffer_cells() {
        let area = Rect::new(0, 0, 8, 2);
        let view = visible_chat_view(&["alpha", "beta"], area);
        let mut buffer = Buffer::empty(area);
        for y in 0..area.height {
            for x in 0..area.width {
                buffer[(x, y)]
                    .set_char(' ')
                    .set_style(Style::default().fg(Color::White));
            }
        }

        render_chat_selection_overlay(
            Some(&view),
            Some(ChatSelection {
                anchor: ChatCellPosition { line: 0, column: 1 },
                focus: ChatCellPosition { line: 1, column: 1 },
            }),
            &mut buffer,
        );

        assert_eq!(buffer[(1, 0)].bg, Color::Cyan);
        assert_eq!(buffer[(1, 0)].fg, Color::Black);
        assert_eq!(buffer[(1, 1)].bg, Color::Cyan);
    }

    #[test]
    fn tab_cycles_slash_command_and_enter_executes_selected_command() {
        let mut harness = test_harness();
        harness.app.state.input = String::from("/s");
        harness.app.state.input_cursor = harness.app.state.input.len();

        harness.app.handle_key_event(key(KeyCode::Tab));
        harness.app.handle_key_event(key(KeyCode::Enter));

        assert_eq!(harness.app.state.mode, UiMode::SessionsMenu);
        assert!(harness.app.state.input.is_empty());

        let requests = harness.drain_requests();
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], RuntimeRequest::ListSessions));
    }

    #[test]
    fn new_command_clears_local_state_without_creating_session() {
        let mut harness = test_harness();
        harness.app.state = populated_state();
        harness.app.state.pending_submit = Some(PendingSubmit {
            session_id: None,
            message: String::from("stale"),
            model_id: String::from("gpt-4o-mini"),
            provider_id: String::from("openai-chat-completions"),
        });

        harness.app.handle_command("new");

        assert_eq!(harness.app.state.mode, UiMode::Chat);
        assert_eq!(harness.app.state.current_session_id, None);
        assert_eq!(harness.app.state.current_tip_id, None);
        assert!(harness.app.state.chat_history.is_empty());
        assert!(harness.app.state.optimistic_messages.is_empty());
        assert!(harness.app.state.pending_submit.is_none());
        assert_eq!(
            harness.app.state.selected_profile_id.as_deref(),
            Some("plan-code")
        );
        assert_eq!(harness.app.state.status, "Started new chat");
        assert!(harness.drain_requests().is_empty());
    }

    #[test]
    fn agent_command_opens_selector_without_session() {
        let mut harness = test_harness();

        harness.app.handle_command("agent");

        assert_eq!(harness.app.state.mode, UiMode::AgentMenu);
        assert_eq!(harness.app.state.agent_profiles.len(), 2);
        assert!(harness.drain_requests().is_empty());
    }

    #[test]
    fn submit_without_session_requests_session_creation() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
        harness.app.state.input = String::from("hello world");
        harness.app.state.input_cursor = harness.app.state.input.len();

        harness.app.handle_key_event(key(KeyCode::Enter));
        assert_eq!(harness.app.state.status, "Creating session");
        assert_eq!(
            harness
                .app
                .state
                .pending_submit
                .as_ref()
                .map(|submit| submit.message.as_str()),
            Some("hello world")
        );

        let requests = harness.drain_requests();
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], RuntimeRequest::CreateSession));
    }

    #[test]
    fn create_session_applies_draft_agent_before_sending() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.selected_profile_id = Some(String::from("build-code"));
        harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
        harness.app.state.input = String::from("hello world");
        harness.app.state.input_cursor = harness.app.state.input.len();

        harness.app.handle_key_event(key(KeyCode::Enter));
        assert!(matches!(
            harness.drain_requests().as_slice(),
            [RuntimeRequest::CreateSession]
        ));

        harness
            .app
            .handle_runtime_response(RuntimeResponse::CreateSession(Ok(String::from("sess-3"))));

        let requests = harness.drain_requests();
        assert!(requests.iter().any(|request| {
            matches!(
                request,
                RuntimeRequest::SetSessionProfile { session_id, profile_id }
                    if session_id == "sess-3" && profile_id == "build-code"
            )
        }));
        assert!(
            !requests
                .iter()
                .any(|request| { matches!(request, RuntimeRequest::SendMessage { .. }) })
        );

        harness
            .app
            .handle_runtime_response(RuntimeResponse::SetSessionProfile {
                profile_id: String::from("build-code"),
                result: Ok(()),
            });

        let requests = harness.drain_requests();
        assert!(requests.iter().any(|request| {
            matches!(
                request,
                RuntimeRequest::SendMessage {
                    session_id,
                    message,
                    model_id,
                    provider_id,
                } if session_id == "sess-3"
                    && message == "hello world"
                    && model_id == "gpt-4o-mini"
                    && provider_id == "openai-chat-completions"
            )
        }));
    }

    #[test]
    fn sessions_menu_new_chat_starts_lazy_draft() {
        let mut harness = test_harness();
        harness.app.state = populated_state();
        harness.app.state.mode = UiMode::SessionsMenu;
        harness.app.state.sessions_menu_index = 0;

        harness
            .app
            .handle_sessions_menu_key_event(key(KeyCode::Enter));

        assert_eq!(harness.app.state.mode, UiMode::Chat);
        assert_eq!(harness.app.state.status, "Started new chat");
        assert!(harness.drain_requests().is_empty());
    }

    #[test]
    fn submit_sends_message_request_and_tracks_optimistic_message() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
        harness.app.state.selected_profile_id = Some(String::from("plan-code"));
        harness.app.state.input = String::from("hello world");
        harness.app.state.input_cursor = harness.app.state.input.len();

        harness.app.handle_key_event(key(KeyCode::Enter));

        assert!(harness.app.state.input.is_empty());
        assert!(harness.app.state.is_streaming);
        assert_eq!(harness.app.state.optimistic_messages.len(), 1);
        assert_eq!(
            harness.app.state.optimistic_messages[0].content,
            "hello world"
        );

        let requests = harness.drain_requests();
        assert_eq!(requests.len(), 1);
        match &requests[0] {
            RuntimeRequest::SendMessage {
                session_id,
                message,
                model_id,
                provider_id,
            } => {
                assert_eq!(session_id, "sess-2");
                assert_eq!(message, "hello world");
                assert_eq!(model_id, "gpt-4o-mini");
                assert_eq!(provider_id, "openai-chat-completions");
            }
            other => panic!("unexpected request: {}", request_name(other)),
        }
    }

    #[test]
    fn submit_unknown_slash_command_sends_message() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
        harness.app.state.selected_profile_id = Some(String::from("plan-code"));
        harness.app.state.input = String::from("/not-a-command");
        harness.app.state.input_cursor = harness.app.state.input.len();

        harness.app.handle_key_event(key(KeyCode::Enter));

        assert!(harness.app.state.input.is_empty());
        assert!(harness.app.state.is_streaming);
        assert_eq!(harness.app.state.optimistic_messages.len(), 1);
        assert_eq!(
            harness.app.state.optimistic_messages[0].content,
            "/not-a-command"
        );

        let requests = harness.drain_requests();
        assert_eq!(requests.len(), 1);
        match &requests[0] {
            RuntimeRequest::SendMessage {
                session_id,
                message,
                model_id,
                provider_id,
            } => {
                assert_eq!(session_id, "sess-2");
                assert_eq!(message, "/not-a-command");
                assert_eq!(model_id, "gpt-4o-mini");
                assert_eq!(provider_id, "openai-chat-completions");
            }
            other => panic!("unexpected request: {}", request_name(other)),
        }
    }

    #[test]
    fn shift_enter_in_chat_inserts_newline_without_submitting() {
        let mut harness = test_harness();
        harness.app.state.input = String::from("hello");
        harness.app.state.input_cursor = harness.app.state.input.len();

        harness.app.handle_key_event(shift_key(KeyCode::Enter));

        assert_eq!(harness.app.state.input, "hello\n");
        assert!(harness.drain_requests().is_empty());
    }

    #[test]
    fn paste_with_newlines_in_chat_inserts_text_without_submitting() {
        let mut harness = test_harness();
        harness.app.state.input = String::from("before ");
        harness.app.state.input_cursor = harness.app.state.input.len();

        let changed = harness
            .app
            .handle_terminal_event(CrosstermEvent::Paste(String::from("line 1\nline 2")));

        assert!(changed);
        assert_eq!(harness.app.state.input, "before line 1\nline 2");
        assert_eq!(
            harness.app.state.input_cursor,
            harness.app.state.input.len()
        );
        assert!(harness.drain_requests().is_empty());
    }

    #[test]
    fn paste_inserts_text_at_cursor_position() {
        let mut harness = test_harness();
        harness.app.state.input = String::from("hello world");
        harness.app.state.input_cursor = "hello ".len();

        let changed = harness
            .app
            .handle_terminal_event(CrosstermEvent::Paste(String::from("big\n")));

        assert!(changed);
        assert_eq!(harness.app.state.input, "hello big\nworld");
        assert_eq!(harness.app.state.input_cursor, "hello big\n".len());
        assert!(harness.drain_requests().is_empty());
    }

    #[test]
    fn escape_closes_providers_editor_before_leaving_advanced_view() {
        let mut harness = test_harness();
        harness.app.state.mode = UiMode::ProvidersMenu;
        harness.app.state.providers_view = ProvidersView::Advanced;
        harness.app.state.providers_advanced_focus = ProvidersAdvancedFocus::ProviderFields;
        harness.app.state.settings_draft = Some(sample_settings());
        harness.app.state.settings_editor =
            Some(ActiveSettingsEditor::Provider(SettingsProviderField::Id));
        harness.app.state.settings_editor_input = String::from("draft-openai");

        harness.app.handle_key_event(key(KeyCode::Esc));
        assert_eq!(harness.app.state.mode, UiMode::ProvidersMenu);
        assert_eq!(harness.app.state.settings_editor, None);
        assert!(harness.app.state.settings_editor_input.is_empty());

        harness.app.handle_key_event(key(KeyCode::Esc));
        assert_eq!(harness.app.state.providers_view, ProvidersView::Detail);
        assert_eq!(harness.app.state.status, "Back to provider detail");
    }

    #[test]
    fn escape_dismisses_command_popup_in_chat() {
        let mut harness = test_harness();
        harness.app.state.input = String::from("/s");
        harness.app.state.input_cursor = harness.app.state.input.len();

        assert!(harness.app.command_popup_visible());

        harness.app.handle_key_event(key(KeyCode::Esc));

        assert_eq!(harness.app.state.mode, UiMode::Chat);
        assert!(harness.app.state.command_popup_dismissed);
        assert!(!harness.app.command_popup_visible());
        assert_eq!(harness.app.state.input, "/s");
    }

    #[test]
    fn escape_dismisses_command_popup_before_cancelling_stream() {
        let mut harness = test_harness();
        harness.app.state.is_streaming = true;
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.input = String::from("/s");
        harness.app.state.input_cursor = harness.app.state.input.len();

        harness.app.handle_key_event(key(KeyCode::Esc));

        assert!(harness.app.state.command_popup_dismissed);
        assert!(harness.app.state.is_streaming);
        assert!(harness.drain_requests().is_empty());
    }

    #[test]
    fn escape_cancels_stream_when_chat_input_is_active() {
        let mut harness = test_harness();
        harness.app.state.is_streaming = true;
        harness.app.state.current_session_id = Some(String::from("sess-2"));

        harness.app.handle_key_event(key(KeyCode::Esc));

        let requests = harness.drain_requests();
        assert!(matches!(
            requests.as_slice(),
            [RuntimeRequest::CancelStream { session_id }] if session_id == "sess-2"
        ));
    }

    #[test]
    fn escape_then_typing_partial_command_submits_message() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
        harness.app.state.selected_profile_id = Some(String::from("plan-code"));
        harness.app.state.input = String::from("/s");
        harness.app.state.input_cursor = harness.app.state.input.len();

        harness.app.handle_key_event(key(KeyCode::Esc));
        harness.app.handle_key_event(key(KeyCode::Char('e')));
        harness.app.handle_key_event(key(KeyCode::Enter));

        assert!(harness.app.state.input.is_empty());
        assert!(harness.app.state.is_streaming);
        assert_eq!(harness.app.state.optimistic_messages.len(), 1);
        assert_eq!(harness.app.state.optimistic_messages[0].content, "/se");

        let requests = harness.drain_requests();
        assert_eq!(requests.len(), 1);
        match &requests[0] {
            RuntimeRequest::SendMessage {
                session_id,
                message,
                model_id,
                provider_id,
            } => {
                assert_eq!(session_id, "sess-2");
                assert_eq!(message, "/se");
                assert_eq!(model_id, "gpt-4o-mini");
                assert_eq!(provider_id, "openai-chat-completions");
            }
            other => panic!("unexpected request: {}", request_name(other)),
        }
    }

    #[test]
    fn submit_while_streaming_queues_message_and_requests_send() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.is_streaming = true;
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
        harness.app.state.selected_profile_id = Some(String::from("plan-code"));
        harness.app.state.input = String::from("keep this draft");
        harness.app.state.input_cursor = harness.app.state.input.len();

        harness.app.handle_key_event(key(KeyCode::Enter));

        assert!(harness.app.state.input.is_empty());
        assert_eq!(harness.app.state.status, "Queued message (1 queued)");
        assert_eq!(harness.app.state.optimistic_messages.len(), 1);
        assert_eq!(harness.app.state.optimistic_messages[0].content, "keep this draft");
        assert!(harness.app.state.optimistic_messages[0].is_queued);

        let requests = harness.drain_requests();
        assert!(matches!(
            requests.as_slice(),
            [RuntimeRequest::SendMessage {
                session_id,
                message,
                model_id,
                provider_id,
            }] if session_id == "sess-2"
                && message == "keep this draft"
                && model_id == "gpt-4o-mini"
                && provider_id == "openai-chat-completions"
        ));
    }

    #[test]
    fn submit_during_tool_execution_queues_message() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.tool_phase = ToolPhase::ExecutingBatch;
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
        harness.app.state.selected_profile_id = Some(String::from("plan-code"));
        harness.app.state.input = String::from("queue this");
        harness.app.state.input_cursor = harness.app.state.input.len();

        harness.app.handle_key_event(key(KeyCode::Enter));

        assert!(harness.app.state.input.is_empty());
        assert_eq!(harness.app.state.status, "Queued message (1 queued)");
        assert_eq!(harness.app.state.optimistic_messages.len(), 1);
        assert_eq!(harness.app.state.optimistic_messages[0].content, "queue this");
        assert!(harness.app.state.optimistic_messages[0].is_queued);

        let requests = harness.drain_requests();
        assert!(matches!(
            requests.as_slice(),
            [RuntimeRequest::SendMessage {
                session_id,
                message,
                model_id,
                provider_id,
            }] if session_id == "sess-2"
                && message == "queue this"
                && model_id == "gpt-4o-mini"
                && provider_id == "openai-chat-completions"
        ));
    }

    #[test]
    fn submit_while_streaming_allows_unbounded_queued_messages() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.is_streaming = true;
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.current_tip_id = Some(String::from("a1"));
        harness.app.state.chat_history = BTreeMap::from([(
            MessageId::new("a1"),
            Message {
                id: MessageId::new("a1"),
                parent_id: None,
                role: ChatRole::Assistant,
                content: String::from("streaming reply"),
                status: MessageStatus::Streaming {
                    call_id: types::CallId::new("call-1"),
                },
                agent_profile_id: Some(String::from("plan-code")),
                tool_state_snapshot: None,
                tool_state_deltas: Vec::new(),
            },
        )]);
        harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
        harness.app.state.selected_profile_id = Some(String::from("plan-code"));

        for idx in 0..3 {
            let text = format!("queued message {idx}");
            harness.app.state.input = text.clone();
            harness.app.state.input_cursor = text.len();
            harness.app.handle_key_event(key(KeyCode::Enter));
        }

        assert_eq!(harness.app.state.optimistic_messages.len(), 3);
        assert!(
            harness
                .app
                .state
                .optimistic_messages
                .iter()
                .all(|message| message.is_queued)
        );
        let rendered = harness.app.state.rendered_messages();
        assert_eq!(rendered[0].role, ChatRole::Assistant);
        assert!(matches!(rendered[0].status, MessageStatus::Streaming { .. }));
        assert_eq!(rendered[1].role, ChatRole::User);
        assert!(rendered[1].content.contains("[queued]"));
        assert!(rendered
            .iter()
            .filter(|message| message.content.contains("[queued]"))
            .count()
            >= 3);
        assert!(harness.app.state.is_streaming);

        let requests = harness.drain_requests();
        let send_count = requests
            .iter()
            .filter(|request| matches!(request, RuntimeRequest::SendMessage { .. }))
            .count();
        assert_eq!(send_count, 3);
    }

    #[test]
    fn settings_command_redirects_to_providers() {
        let mut harness = test_harness();
        harness.app.handle_command("settings");

        assert_eq!(
            harness.app.state.status,
            "Unknown command: /settings. Use /providers"
        );
        assert!(harness.drain_requests().is_empty());
    }

    #[test]
    fn final_tool_decision_starts_batch_execution() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.tool_phase = ToolPhase::Deciding;
        harness.app.state.pending_tools = vec![PendingTool {
            call_id: String::from("call-2"),
            tool_id: String::from("write_file"),
            args: String::from("{\"path\":\"src/app.rs\",\"content\":\"...\"}"),
            description: String::from("Patch the app module"),
            risk_level: String::from("undoable_workspace_write"),
            reasons: vec![String::from("Updates tracked source code")],
            approved: None,
            queue_order: 0,
        }];

        harness.app.handle_key_event(key(KeyCode::Enter));

        assert_eq!(harness.app.state.tool_phase, ToolPhase::Deciding);
        let requests = harness.drain_requests();
        assert_eq!(requests.len(), 1);
        assert!(matches!(
            &requests[0],
            RuntimeRequest::ApproveTool { session_id, call_id }
                if session_id == "sess-2" && call_id == "call-2"
        ));
    }

    #[test]
    fn settings_save_error_populates_field_errors() {
        let mut harness = test_harness();
        harness.app.state.mode = UiMode::ProvidersMenu;
        harness
            .app
            .handle_runtime_response(RuntimeResponse::SaveSettings(Err(String::from(
                "providers[0].id: duplicate provider\nmodels[0].max_context: invalid context",
            ))));

        assert_eq!(
            harness.app.state.settings_errors.get("providers[0].id"),
            Some(&String::from("duplicate provider"))
        );
        assert_eq!(
            harness
                .app
                .state
                .settings_errors
                .get("models[0].max_context"),
            Some(&String::from("invalid context"))
        );
        assert!(
            harness
                .app
                .state
                .status
                .starts_with("Failed saving settings:")
        );
    }

    #[test]
    fn chat_history_response_reconciles_matching_optimistic_messages() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-1"));
        harness
            .app
            .state
            .optimistic_messages
            .push(OptimisticMessage {
                local_id: String::from("local-user-1"),
                content: String::from("hello world"),
                content_key: String::from("hello world"),
                occurrence: 1,
                is_queued: false,
            });

        harness
            .app
            .handle_runtime_response(RuntimeResponse::ChatHistory {
                session_id: String::from("sess-1"),
                result: Ok(BTreeMap::from([(
                    MessageId::new("m1"),
                    message("m1", None, ChatRole::User, "hello world"),
                )])),
            });

        assert!(harness.app.state.optimistic_messages.is_empty());
        assert_eq!(harness.app.state.chat_history.len(), 1);
    }

    #[test]
    fn reconcile_optimistic_messages_keeps_duplicate_content_until_occurrence_matches() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-1"));
        harness.app.state.current_tip_id = Some(String::from("m1"));
        harness.app.state.chat_history = BTreeMap::from([(
            MessageId::new("m1"),
            message("m1", None, ChatRole::User, "hello"),
        )]);
        harness
            .app
            .state
            .optimistic_messages
            .push(OptimisticMessage {
                local_id: String::from("local-user-1"),
                content: String::from("hello"),
                content_key: String::from("hello"),
                occurrence: 2,
                is_queued: true,
            });

        harness
            .app
            .handle_runtime_response(RuntimeResponse::ChatHistory {
                session_id: String::from("sess-1"),
                result: Ok(BTreeMap::from([(
                    MessageId::new("m1"),
                    message("m1", None, ChatRole::User, "hello"),
                )])),
            });

        assert_eq!(harness.app.state.optimistic_messages.len(), 1);

        harness
            .app
            .handle_runtime_response(RuntimeResponse::ChatHistory {
                session_id: String::from("sess-1"),
                result: Ok(BTreeMap::from([
                    (MessageId::new("m1"), message("m1", None, ChatRole::User, "hello")),
                    (
                        MessageId::new("m2"),
                        message("m2", Some("m1"), ChatRole::User, "hello"),
                    ),
                ])),
            });

        assert!(harness.app.state.optimistic_messages.is_empty());
    }

    #[test]
    fn queued_status_updates_after_reconciliation() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-1"));
        harness.app.state.status = String::from("Queued message (1 queued)");
        harness
            .app
            .state
            .optimistic_messages
            .push(OptimisticMessage {
                local_id: String::from("local-user-1"),
                content: String::from("hello world"),
                content_key: String::from("hello world"),
                occurrence: 1,
                is_queued: true,
            });

        harness
            .app
            .handle_runtime_response(RuntimeResponse::ChatHistory {
                session_id: String::from("sess-1"),
                result: Ok(BTreeMap::from([(
                    MessageId::new("m1"),
                    message("m1", None, ChatRole::User, "hello world"),
                )])),
            });

        assert!(harness.app.state.optimistic_messages.is_empty());
        assert_eq!(harness.app.state.status, "Queued messages sent");
    }

    #[test]
    fn stale_chat_history_response_is_ignored() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.chat_history = BTreeMap::from([(
            MessageId::new("m-current"),
            message("m-current", None, ChatRole::Assistant, "current"),
        )]);

        harness
            .app
            .handle_runtime_response(RuntimeResponse::ChatHistory {
                session_id: String::from("sess-1"),
                result: Ok(BTreeMap::from([(
                    MessageId::new("m-stale"),
                    message("m-stale", None, ChatRole::Assistant, "stale"),
                )])),
            });

        assert!(
            harness
                .app
                .state
                .chat_history
                .contains_key(&MessageId::new("m-current"))
        );
        assert!(
            !harness
                .app
                .state
                .chat_history
                .contains_key(&MessageId::new("m-stale"))
        );
    }

    #[test]
    fn stale_current_tip_response_is_ignored() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.current_tip_id = Some(String::from("tip-current"));

        harness
            .app
            .handle_runtime_response(RuntimeResponse::CurrentTip {
                session_id: String::from("sess-1"),
                result: Ok(Some(String::from("tip-stale"))),
            });

        assert_eq!(
            harness.app.state.current_tip_id.as_deref(),
            Some("tip-current")
        );
    }

    #[test]
    fn pending_tools_response_for_background_session_is_ignored() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.pending_tools = sample_pending_tools();

        harness
            .app
            .handle_runtime_response(RuntimeResponse::PendingTools {
                session_id: String::from("sess-1"),
                result: Ok(vec![agent_runtime::PendingToolInfo {
                    call_id: String::from("call-other"),
                    tool_id: String::from("read_file"),
                    args: String::from("{}"),
                    description: String::from("other"),
                    risk_level: String::from("read_only_workspace"),
                    reasons: Vec::new(),
                    approved: None,
                    queue_order: 0,
                }]),
            });

        assert_eq!(harness.app.state.pending_tools.len(), 2);
        assert_eq!(harness.app.state.pending_tools[0].call_id, "call-1");
    }

    #[test]
    fn settings_response_opens_editor_with_clean_state() {
        let mut harness = test_harness();
        harness.app.state.settings_errors =
            HashMap::from([(String::from("providers[0].id"), String::from("old error"))]);
        harness.app.state.settings_editor = Some(ActiveSettingsEditor::Model(
            SettingsModelField::Value(String::from("name")),
        ));
        harness.app.state.settings_editor_input = String::from("stale");
        harness.app.state.settings_delete_armed = true;

        harness
            .app
            .handle_runtime_response(RuntimeResponse::Settings(Ok(sample_settings())));

        assert_eq!(harness.app.state.mode, UiMode::ProvidersMenu);
        assert_eq!(harness.app.state.providers_view, ProvidersView::List);
        assert_eq!(harness.app.state.status, "Providers loaded");
        assert!(harness.app.state.settings_errors.is_empty());
        assert_eq!(harness.app.state.settings_editor, None);
        assert!(harness.app.state.settings_editor_input.is_empty());
        assert!(!harness.app.state.settings_delete_armed);
    }

    #[test]
    fn providers_command_requests_settings_definitions_and_auth_status() {
        let mut harness = test_harness();

        harness.app.handle_command("providers");

        assert_eq!(harness.app.state.mode, UiMode::ProvidersMenu);
        assert_eq!(harness.app.state.providers_view, ProvidersView::List);
        let requests = harness.drain_requests();
        assert_eq!(requests.len(), 3);
        assert!(matches!(
            requests[0],
            RuntimeRequest::ListProviderDefinitions
        ));
        assert!(matches!(requests[1], RuntimeRequest::GetSettings));
        assert!(matches!(
            requests[2],
            RuntimeRequest::GetOpenAiCodexAuthStatus
        ));
    }

    #[test]
    fn connect_provider_autosaves_new_provider() {
        let mut harness = test_harness();
        harness.app.state.settings_draft = Some(sample_settings());
        harness.app.state.provider_definitions = sample_provider_definitions();
        harness.app.state.mode = UiMode::ProvidersMenu;
        harness.app.state.providers_view = ProvidersView::Connect;

        harness.app.handle_key_event(key(KeyCode::Enter));

        let requests = harness.drain_requests();
        assert!(requests.iter().any(|request| matches!(
            request,
            RuntimeRequest::SaveSettings { settings }
                if settings.providers.iter().any(|provider| provider.type_id == "openai-codex")
        )));
    }

    #[test]
    fn provider_field_commit_autosaves_settings() {
        let mut harness = test_harness();
        harness.app.state.settings_draft = Some(sample_settings());
        harness.app.state.provider_definitions = sample_provider_definitions();
        harness.app.state.mode = UiMode::ProvidersMenu;
        harness.app.state.providers_view = ProvidersView::Advanced;
        harness.app.state.settings_editor =
            Some(ActiveSettingsEditor::Provider(SettingsProviderField::Id));
        harness.app.state.settings_editor_input = String::from("renamed-provider");

        harness.app.handle_key_event(key(KeyCode::Enter));

        let requests = harness.drain_requests();
        assert!(requests.iter().any(|request| matches!(
            request,
            RuntimeRequest::SaveSettings { settings }
                if settings.providers.iter().any(|provider| provider.id == "renamed-provider")
        )));
    }

    #[test]
    fn auth_updated_event_refreshes_openai_status() {
        let mut harness = test_harness();

        harness
            .app
            .handle_runtime_event(Event::OpenAiCodexAuthUpdated {
                status: agent_runtime::OpenAiCodexAuthStatus {
                    state: agent_runtime::OpenAiCodexLoginState::Authenticated,
                    email: Some(String::from("dev@example.com")),
                    plan_type: Some(String::from("Pro")),
                    account_id: Some(String::from("acct_123")),
                    last_refresh_unix: Some(42),
                    error: None,
                },
            });

        assert_eq!(
            harness.app.state.openai_codex_auth.state,
            ProviderAuthState::Authenticated
        );
        assert_eq!(
            harness.app.state.openai_codex_auth.plan_type.as_deref(),
            Some("Pro")
        );
    }

    #[test]
    fn approve_tool_response_marks_pending_tool_as_approved() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.pending_tools = sample_pending_tools();
        harness.app.state.tool_phase = ToolPhase::Deciding;

        harness
            .app
            .handle_runtime_response(RuntimeResponse::ApproveTool {
                call_id: String::from("call-2"),
                result: Ok(()),
            });

        assert_eq!(harness.app.state.pending_tools[1].approved, Some(true));
        assert_eq!(harness.app.state.tool_phase, ToolPhase::ExecutingBatch);
        let requests = harness.drain_requests();
        assert!(matches!(
            requests.as_slice(),
            [RuntimeRequest::ExecuteApprovedTools { session_id }] if session_id == "sess-2"
        ));
    }

    #[test]
    fn switching_sessions_clears_foreground_tool_state_and_requests_fresh_data() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-1"));
        harness.app.state.pending_tools = sample_pending_tools();

        harness
            .app
            .handle_runtime_response(RuntimeResponse::LoadSession {
                session_id: String::from("sess-2"),
                result: Ok(true),
            });

        assert_eq!(
            harness.app.state.current_session_id.as_deref(),
            Some("sess-2")
        );
        assert!(harness.app.state.pending_tools.is_empty());

        let requests = harness.drain_requests();
        assert!(requests
            .iter()
            .any(|request| matches!(request, RuntimeRequest::GetCurrentTip { session_id } if session_id == "sess-2")));
        assert!(requests
            .iter()
            .any(|request| matches!(request, RuntimeRequest::GetChatHistory { session_id } if session_id == "sess-2")));
        assert!(requests
            .iter()
            .any(|request| matches!(request, RuntimeRequest::GetPendingTools { session_id } if session_id == "sess-2")));
    }

    #[test]
    fn failed_tool_result_for_current_session_adds_optimistic_tool_message() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.pending_tools = sample_pending_tools();

        harness.app.handle_runtime_event(Event::ToolResultReady {
            session_id: String::from("sess-2"),
            call_id: String::from("call-2"),
            tool_id: String::from("write_file"),
            success: false,
            output: String::from("{\"error\":\"boom\"}"),
            denied: false,
        });

        assert_eq!(harness.app.state.pending_tools.len(), 1);
        assert_eq!(harness.app.state.optimistic_tool_messages.len(), 1);
        assert!(
            harness.app.state.optimistic_tool_messages[0]
                .content
                .contains("\"error\": \"boom\"")
        );
    }

    #[test]
    fn tool_result_for_background_session_does_not_cache_tool_message() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.pending_tools = sample_pending_tools();

        harness.app.handle_runtime_event(Event::ToolResultReady {
            session_id: String::from("sess-1"),
            call_id: String::from("call-bg"),
            tool_id: String::from("write_file"),
            success: false,
            output: String::from("{\"error\":\"boom\"}"),
            denied: false,
        });

        assert!(harness.app.state.optimistic_tool_messages.is_empty());
        let requests = harness.drain_requests();
        assert!(
            requests
                .iter()
                .any(|request| matches!(request, RuntimeRequest::ListSessions))
        );
    }

    #[test]
    fn chat_history_response_reconciles_matching_optimistic_tool_messages() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-1"));
        harness
            .app
            .state
            .optimistic_tool_messages
            .push(OptimisticToolMessage {
                local_id: String::from("local-tool-1"),
                content: String::from("Tool 'write_file' result:\n{\n  \"error\": \"boom\"\n}"),
            });

        harness
            .app
            .handle_runtime_response(RuntimeResponse::ChatHistory {
                session_id: String::from("sess-1"),
                result: Ok(BTreeMap::from([(
                    MessageId::new("m1"),
                    message(
                        "m1",
                        None,
                        ChatRole::Tool,
                        "Tool 'write_file' result:\n{\n  \"error\": \"boom\"\n}",
                    ),
                )])),
            });

        assert!(harness.app.state.optimistic_tool_messages.is_empty());
    }

    #[test]
    fn tool_call_event_adds_pending_tool_and_status() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-2"));

        harness.app.handle_runtime_event(Event::ToolCallDetected {
            session_id: String::from("sess-2"),
            call_id: String::from("call-3"),
            tool_id: String::from("read_file"),
            args: String::from("{\"path\":\"Cargo.toml\"}"),
            description: String::from("Read the workspace manifest"),
            risk_level: String::from("read_only_workspace"),
            reasons: vec![String::from("Reads local config")],
            queue_order: 0,
        });

        assert_eq!(harness.app.state.pending_tools.len(), 1);
        assert_eq!(harness.app.state.tool_phase, ToolPhase::Deciding);
        assert_eq!(harness.app.state.status, "1 tool call(s) pending");
    }

    #[test]
    fn tool_call_for_background_session_only_refreshes_sessions() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-2"));

        harness.app.handle_runtime_event(Event::ToolCallDetected {
            session_id: String::from("sess-1"),
            call_id: String::from("call-3"),
            tool_id: String::from("read_file"),
            args: String::from("{\"path\":\"Cargo.toml\"}"),
            description: String::from("Read the workspace manifest"),
            risk_level: String::from("read_only_workspace"),
            reasons: vec![String::from("Reads local config")],
            queue_order: 0,
        });

        assert!(harness.app.state.pending_tools.is_empty());
        let requests = harness.drain_requests();
        assert!(
            requests
                .iter()
                .any(|request| matches!(request, RuntimeRequest::ListSessions))
        );
    }

    #[test]
    fn stream_cancelled_event_refreshes_foreground_session_and_sessions_list() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.is_streaming = true;

        harness.app.handle_runtime_event(Event::StreamCancelled {
            session_id: String::from("sess-2"),
            message_id: String::from("m-cancelled"),
        });

        assert!(!harness.app.state.is_streaming);
        assert_eq!(harness.app.state.status, "Stream cancelled");
        let requests = harness.drain_requests();
        assert!(requests.iter().any(|request| matches!(
            request,
            RuntimeRequest::GetCurrentTip { session_id } if session_id == "sess-2"
        )));
        assert!(requests.iter().any(|request| matches!(
            request,
            RuntimeRequest::GetChatHistory { session_id } if session_id == "sess-2"
        )));
        assert!(requests.iter().any(|request| matches!(
            request,
            RuntimeRequest::GetPendingTools { session_id } if session_id == "sess-2"
        )));
        assert!(
            requests
                .iter()
                .any(|request| matches!(request, RuntimeRequest::ListSessions))
        );
    }

    #[test]
    fn sessions_menu_renders_streaming_suffix() {
        let state = AppState {
            mode: UiMode::SessionsMenu,
            sessions: sample_sessions(),
            ..AppState::default()
        };

        let snapshot = render_state_snapshot(&state, 80, 12);

        assert!(snapshot.contains("[streaming]"));
    }

    fn request_name(request: &RuntimeRequest) -> &'static str {
        match request {
            RuntimeRequest::ListModels => "ListModels",
            RuntimeRequest::ListAgentProfiles { .. } => "ListAgentProfiles",
            RuntimeRequest::ListProviderDefinitions => "ListProviderDefinitions",
            RuntimeRequest::GetSettings => "GetSettings",
            RuntimeRequest::GetOpenAiCodexAuthStatus => "GetOpenAiCodexAuthStatus",
            RuntimeRequest::StartOpenAiCodexBrowserLogin => "StartOpenAiCodexBrowserLogin",
            RuntimeRequest::StartOpenAiCodexDeviceCodeLogin => "StartOpenAiCodexDeviceCodeLogin",
            RuntimeRequest::CancelOpenAiCodexLogin => "CancelOpenAiCodexLogin",
            RuntimeRequest::LogoutOpenAiCodexAuth => "LogoutOpenAiCodexAuth",
            RuntimeRequest::CreateSession => "CreateSession",
            RuntimeRequest::SetSessionProfile { .. } => "SetSessionProfile",
            RuntimeRequest::SendMessage { .. } => "SendMessage",
            RuntimeRequest::SaveSettings { .. } => "SaveSettings",
            RuntimeRequest::GetChatHistory { .. } => "GetChatHistory",
            RuntimeRequest::GetCurrentTip { .. } => "GetCurrentTip",
            RuntimeRequest::GetPendingTools { .. } => "GetPendingTools",
            RuntimeRequest::LoadSession { .. } => "LoadSession",
            RuntimeRequest::ListSessions => "ListSessions",
            RuntimeRequest::DeleteSession { .. } => "DeleteSession",
            RuntimeRequest::ApproveTool { .. } => "ApproveTool",
            RuntimeRequest::DenyTool { .. } => "DenyTool",
            RuntimeRequest::CancelStream { .. } => "CancelStream",
            RuntimeRequest::ExecuteApprovedTools { .. } => "ExecuteApprovedTools",
        }
    }
}
