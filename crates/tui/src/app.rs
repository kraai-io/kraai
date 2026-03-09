use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_runtime::{
    Event, Model, ModelSettings, ProviderSettings, ProviderType, RuntimeHandle, Session,
    SettingsDocument,
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
use types::{ChatRole, Message, MessageId, MessageStatus};

use crate::components::{ChatHistory, RenderedLine, TextInput, VisibleChatView};

mod runtime_bridge;
mod ui;

use self::runtime_bridge::spawn_runtime_bridge;
use self::ui::{
    SETTINGS_MODEL_FIELDS, active_command_prefix, adjust_index, copy_via_osc52, is_copy_shortcut,
    is_known_slash_command, model_menu_next_index, model_menu_previous_index,
    parse_settings_errors, selection_text, slash_command_matches,
};
#[cfg(test)]
use self::ui::{menu_scroll_offset, render_chat_selection_overlay};

const SLASH_COMMANDS: [(&str, &str); 7] = [
    ("help", "Open command help"),
    ("model", "Open model selector"),
    ("new", "Start new session"),
    ("quit", "Exit the TUI"),
    ("settings", "Open settings editor"),
    ("sessions", "Open sessions menu"),
    ("tools", "Open tools approval menu"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum UiMode {
    Chat,
    ModelMenu,
    SettingsMenu,
    SessionsMenu,
    ToolsMenu,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsFocus {
    ProviderList,
    ProviderForm,
    ModelList,
    ModelForm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsProviderField {
    Id,
    Type,
    BaseUrl,
    ApiKey,
    EnvVarApiKey,
    OnlyListedModels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsModelField {
    Id,
    Name,
    MaxContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveSettingsEditor {
    Provider(SettingsProviderField),
    Model(SettingsModelField),
}

#[derive(Clone, Debug)]
struct PendingTool {
    session_id: String,
    call_id: String,
    tool_id: String,
    args: String,
    description: String,
    risk_level: String,
    reasons: Vec<String>,
    approved: Option<bool>,
}

#[derive(Clone, Debug)]
struct OptimisticMessage {
    local_id: String,
    content: String,
}

#[derive(Clone, Debug)]
struct PendingSubmit {
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
    GetSettings,
    CreateSession,
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
    ExecuteApprovedTools {
        session_id: String,
    },
}

enum RuntimeResponse {
    Models(Result<HashMap<String, Vec<Model>>, String>),
    Settings(Result<SettingsDocument, String>),
    CreateSession(Result<String, String>),
    SendMessage(Result<(), String>),
    SaveSettings(Result<(), String>),
    ChatHistory(Result<BTreeMap<MessageId, Message>, String>),
    CurrentTip(Result<Option<String>, String>),
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

pub struct AppState {
    exit: bool,
    input: String,
    input_cursor: usize,
    ctrl_c_exit_armed: bool,
    chat_history: BTreeMap<MessageId, Message>,
    optimistic_messages: Vec<OptimisticMessage>,
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
    selected_provider_id: Option<String>,
    selected_model_id: Option<String>,
    pending_tools: Vec<PendingTool>,
    sessions: Vec<Session>,
    current_session_id: Option<String>,
    current_tip_id: Option<String>,
    model_menu_index: usize,
    sessions_menu_index: usize,
    tools_menu_index: usize,
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
            selected_provider_id: None,
            selected_model_id: None,
            pending_tools: Vec::new(),
            sessions: Vec::new(),
            current_session_id: None,
            current_tip_id: None,
            model_menu_index: 0,
            sessions_menu_index: 0,
            tools_menu_index: 0,
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

            terminal.draw(|frame| {
                let area = frame.area();
                if self.state.mode == UiMode::Chat {
                    let input_height = TextInput::new(&self.state.input, self.state.input_cursor)
                        .get_height(area.width);
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

                if self.state.mode == UiMode::Chat {
                    let input_height = TextInput::new(&self.state.input, self.state.input_cursor)
                        .get_height(area.width);
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
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
            }
            Event::StreamError {
                session_id, error, ..
            } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                self.state.is_streaming = false;
                self.last_stream_refresh = None;
                self.state.status = format!("Stream error: {error}");
            }
            Event::HistoryUpdated { session_id } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.clamp_chat_scroll();
                    self.request_sync_for_session(&session_id);
                } else {
                    self.request(RuntimeRequest::ListSessions);
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
            } => {
                let exists = self
                    .state
                    .pending_tools
                    .iter()
                    .any(|tool| tool.call_id == call_id);
                if !exists {
                    self.state.pending_tools.push(PendingTool {
                        session_id,
                        call_id,
                        tool_id,
                        args,
                        description,
                        risk_level,
                        reasons,
                        approved: None,
                    });
                    self.state.status = format!(
                        "{} tool call(s) pending. Use /tools",
                        self.current_session_pending_tools().len()
                    );
                }
            }
            Event::ToolResultReady {
                session_id,
                call_id,
                tool_id,
                success,
                denied,
                ..
            } => {
                self.state
                    .pending_tools
                    .retain(|tool| !(tool.session_id == session_id && tool.call_id == call_id));
                self.state.status = if denied {
                    format!("Tool denied: {tool_id}")
                } else if success {
                    format!("Tool succeeded: {tool_id}")
                } else {
                    format!("Tool failed: {tool_id}")
                };
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
                self.state.mode = UiMode::SettingsMenu;
                self.state.status = String::from("Editing settings");
            }
            RuntimeResponse::Settings(Err(err)) => {
                self.state.status = format!("Failed loading settings: {err}");
            }
            RuntimeResponse::CreateSession(Ok(session_id)) => {
                self.reset_chat_session(Some(session_id.clone()), "Session ready");
                self.request_sync_for_session(&session_id);

                if let Some(pending_submit) = self.state.pending_submit.take() {
                    self.dispatch_send_message(
                        session_id,
                        pending_submit.message,
                        pending_submit.model_id,
                        pending_submit.provider_id,
                    );
                }
            }
            RuntimeResponse::CreateSession(Err(err)) => {
                self.state.pending_submit = None;
                self.state.status = format!("Failed creating session: {err}");
            }
            RuntimeResponse::SendMessage(Ok(())) => {}
            RuntimeResponse::SendMessage(Err(err)) => {
                self.state.is_streaming = false;
                self.state.status = format!("Send failed: {err}");
            }
            RuntimeResponse::SaveSettings(Ok(())) => {
                self.state.settings_errors.clear();
                self.state.settings_delete_armed = false;
                self.state.settings_editor = None;
                self.state.settings_editor_input.clear();
                self.state.mode = UiMode::Chat;
                self.state.status = String::from("Settings saved");
            }
            RuntimeResponse::SaveSettings(Err(err)) => {
                self.state.settings_errors = parse_settings_errors(&err);
                self.state.status = format!("Failed saving settings: {err}");
            }
            RuntimeResponse::ChatHistory(Ok(history)) => {
                self.state.chat_history = history;
                self.invalidate_chat_cache();
                self.reconcile_optimistic_messages();
                self.clamp_chat_scroll();
            }
            RuntimeResponse::ChatHistory(Err(err)) => {
                self.state.status = format!("Failed loading history: {err}");
            }
            RuntimeResponse::CurrentTip(Ok(tip)) => {
                if self.state.current_tip_id != tip {
                    self.state.current_tip_id = tip;
                    self.invalidate_chat_cache();
                    self.reconcile_optimistic_messages();
                    self.clamp_chat_scroll();
                }
            }
            RuntimeResponse::CurrentTip(Err(err)) => {
                self.state.status = format!("Failed loading tip: {err}");
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
            }
            RuntimeResponse::DenyTool {
                result: Err(err), ..
            } => {
                self.state.status = format!("Failed denying tool: {err}");
            }
            RuntimeResponse::ExecuteApprovedTools(Ok(())) => {
                self.state.status = String::from("Executing approved tools");
            }
            RuntimeResponse::ExecuteApprovedTools(Err(err)) => {
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
        if self.state.mode != UiMode::Chat {
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
        if matches!(key_event.code, KeyCode::Esc) {
            if self.state.mode == UiMode::Chat && self.command_popup_visible() {
                self.state.command_popup_dismissed = true;
                self.reset_completion_cycle();
                return;
            }
            if self.state.mode == UiMode::SettingsMenu {
                if self.state.settings_editor.take().is_some() {
                    self.state.settings_editor_input.clear();
                } else {
                    self.state.mode = UiMode::Chat;
                    self.state.status = String::from("Settings editor closed");
                }
                return;
            }
            self.clear_chat_selection();
            self.state.visible_chat_view = None;
            self.state.mode = UiMode::Chat;
            return;
        }

        match self.state.mode {
            UiMode::Chat => self.handle_chat_key_event(key_event),
            UiMode::ModelMenu => self.handle_model_menu_key_event(key_event),
            UiMode::SettingsMenu => self.handle_settings_key_event(key_event),
            UiMode::SessionsMenu => self.handle_sessions_menu_key_event(key_event),
            UiMode::ToolsMenu => self.handle_tools_menu_key_event(key_event),
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
        if self.state.mode != UiMode::Chat || text.is_empty() {
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
                    self.start_new_chat_draft();
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

    fn handle_tools_menu_key_event(&mut self, key_event: KeyEvent) {
        let current_pending_tools = self.current_session_pending_tools();
        let len = current_pending_tools.len();

        match key_event.code {
            KeyCode::Up => {
                self.state.tools_menu_index = self.state.tools_menu_index.saturating_sub(1);
            }
            KeyCode::Down => {
                if len > 0 {
                    self.state.tools_menu_index = (self.state.tools_menu_index + 1).min(len - 1);
                }
            }
            KeyCode::Char('a') => {
                if let Some(tool) = current_pending_tools.get(self.state.tools_menu_index)
                    && let Some(session_id) = &self.state.current_session_id
                {
                    self.request(RuntimeRequest::ApproveTool {
                        session_id: session_id.clone(),
                        call_id: tool.call_id.clone(),
                    });
                }
            }
            KeyCode::Char('d') => {
                if let Some(tool) = current_pending_tools.get(self.state.tools_menu_index)
                    && let Some(session_id) = &self.state.current_session_id
                {
                    self.request(RuntimeRequest::DenyTool {
                        session_id: session_id.clone(),
                        call_id: tool.call_id.clone(),
                    });
                }
            }
            KeyCode::Char('e') => {
                if current_pending_tools
                    .iter()
                    .any(|tool| tool.approved == Some(true))
                    && let Some(session_id) = &self.state.current_session_id
                {
                    self.request(RuntimeRequest::ExecuteApprovedTools {
                        session_id: session_id.clone(),
                    });
                    self.state.mode = UiMode::Chat;
                }
            }
            _ => {}
        }
    }

    fn handle_settings_key_event(&mut self, key_event: KeyEvent) {
        if self.state.settings_editor.is_some() {
            self.handle_settings_editor_key_event(key_event);
            return;
        }

        match key_event.code {
            KeyCode::Tab => self.cycle_settings_focus(true),
            KeyCode::BackTab => self.cycle_settings_focus(false),
            KeyCode::Up => self.move_settings_selection(-1),
            KeyCode::Down => self.move_settings_selection(1),
            KeyCode::Left => self.adjust_settings_field(false),
            KeyCode::Right => self.adjust_settings_field(true),
            KeyCode::Enter => self.activate_settings_field(),
            KeyCode::Char('a') => self.add_settings_item(),
            KeyCode::Char('x') => self.delete_settings_item(),
            KeyCode::Char('s') => self.save_settings_draft(),
            KeyCode::Char(' ') => self.toggle_settings_field(),
            _ => {}
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

    fn cycle_settings_focus(&mut self, forward: bool) {
        self.state.settings_delete_armed = false;
        self.state.settings_focus = match (self.state.settings_focus, forward) {
            (SettingsFocus::ProviderList, true) => SettingsFocus::ProviderForm,
            (SettingsFocus::ProviderForm, true) => SettingsFocus::ModelList,
            (SettingsFocus::ModelList, true) => SettingsFocus::ModelForm,
            (SettingsFocus::ModelForm, true) => SettingsFocus::ProviderList,
            (SettingsFocus::ProviderList, false) => SettingsFocus::ModelForm,
            (SettingsFocus::ProviderForm, false) => SettingsFocus::ProviderList,
            (SettingsFocus::ModelList, false) => SettingsFocus::ProviderForm,
            (SettingsFocus::ModelForm, false) => SettingsFocus::ModelList,
        };
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
                let len = SETTINGS_MODEL_FIELDS.len();
                self.state.settings_model_field_index =
                    adjust_index(self.state.settings_model_field_index, len, delta);
            }
        }
    }

    fn adjust_settings_field(&mut self, forward: bool) {
        match self.state.settings_focus {
            SettingsFocus::ProviderForm => {
                if self.current_provider_field() == Some(SettingsProviderField::Type) {
                    self.cycle_provider_type(forward);
                } else if self.current_provider_field()
                    == Some(SettingsProviderField::OnlyListedModels)
                {
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
                Some(SettingsProviderField::Type) => self.cycle_provider_type(true),
                Some(SettingsProviderField::OnlyListedModels) => self.toggle_settings_field(),
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
        if self.current_provider_field() != Some(SettingsProviderField::OnlyListedModels) {
            return;
        }

        let provider_index = self.state.settings_provider_index;
        if let Some(provider) = self
            .state
            .settings_draft
            .as_mut()
            .and_then(|draft| draft.providers.get_mut(provider_index))
        {
            provider.only_listed_models = !provider.only_listed_models;
        }
    }

    fn add_settings_item(&mut self) {
        self.state.settings_delete_armed = false;
        match self.state.settings_focus {
            SettingsFocus::ProviderList | SettingsFocus::ProviderForm => {
                if let Some(draft) = self.state.settings_draft.as_mut() {
                    let next_index = draft.providers.len();
                    draft.providers.push(ProviderSettings {
                        id: format!("provider-{}", next_index + 1),
                        provider_type: ProviderType::OpenAi,
                        base_url: Some(String::from("https://api.openai.com/v1")),
                        api_key: None,
                        env_var_api_key: Some(String::from("OPENAI_API_KEY")),
                        only_listed_models: true,
                    });
                    self.state.settings_provider_index = next_index;
                    self.state.settings_model_index = 0;
                    self.state.status = String::from("Added provider");
                }
            }
            SettingsFocus::ModelList | SettingsFocus::ModelForm => {
                let provider_id = self.current_provider().map(|provider| provider.id.clone());
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
                        name: None,
                        max_context: None,
                    });
                    self.state.settings_model_index = next_count;
                    self.state.status = String::from("Added model");
                }
            }
        }
    }

    fn delete_settings_item(&mut self) {
        if !self.state.settings_delete_armed {
            self.state.settings_delete_armed = true;
            self.state.status = String::from("Press x again to confirm delete");
            return;
        }

        self.state.settings_delete_armed = false;
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
                }
            }
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
        let value = match field {
            SettingsProviderField::Id => provider.id.clone(),
            SettingsProviderField::BaseUrl => provider.base_url.clone().unwrap_or_default(),
            SettingsProviderField::ApiKey => provider.api_key.clone().unwrap_or_default(),
            SettingsProviderField::EnvVarApiKey => {
                provider.env_var_api_key.clone().unwrap_or_default()
            }
            SettingsProviderField::Type | SettingsProviderField::OnlyListedModels => return,
        };
        self.state.settings_editor = Some(ActiveSettingsEditor::Provider(field));
        self.state.settings_editor_input = value;
    }

    fn start_model_editor(&mut self, field: SettingsModelField) {
        let Some(model) = self.current_model() else {
            return;
        };
        let value = match field {
            SettingsModelField::Id => model.id.clone(),
            SettingsModelField::Name => model.name.clone().unwrap_or_default(),
            SettingsModelField::MaxContext => model
                .max_context
                .map(|value| value.to_string())
                .unwrap_or_default(),
        };
        self.state.settings_editor = Some(ActiveSettingsEditor::Model(field));
        self.state.settings_editor_input = value;
    }

    fn commit_settings_editor(&mut self) {
        let Some(editor) = self.state.settings_editor.take() else {
            return;
        };
        let value = self.state.settings_editor_input.trim().to_string();

        match editor {
            ActiveSettingsEditor::Provider(field) => {
                let provider_index = self.state.settings_provider_index;
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
                        }
                        SettingsProviderField::BaseUrl => {
                            provider.base_url = if value.is_empty() { None } else { Some(value) };
                        }
                        SettingsProviderField::ApiKey => {
                            provider.api_key = if value.is_empty() { None } else { Some(value) };
                        }
                        SettingsProviderField::EnvVarApiKey => {
                            provider.env_var_api_key =
                                if value.is_empty() { None } else { Some(value) };
                        }
                        SettingsProviderField::Type | SettingsProviderField::OnlyListedModels => {}
                    }
                }
            }
            ActiveSettingsEditor::Model(field) => {
                if let Some(global_index) = self.current_model_global_index()
                    && let Some(model) = self
                        .state
                        .settings_draft
                        .as_mut()
                        .and_then(|draft| draft.models.get_mut(global_index))
                {
                    match field {
                        SettingsModelField::Id => model.id = value,
                        SettingsModelField::Name => {
                            model.name = if value.is_empty() { None } else { Some(value) };
                        }
                        SettingsModelField::MaxContext => {
                            model.max_context = value.parse::<u32>().ok();
                        }
                    }
                }
            }
        }

        self.state.settings_editor_input.clear();
    }

    fn cycle_provider_type(&mut self, forward: bool) {
        let _ = forward;
        let provider_index = self.state.settings_provider_index;
        if let Some(provider) = self
            .state
            .settings_draft
            .as_mut()
            .and_then(|draft| draft.providers.get_mut(provider_index))
        {
            provider.provider_type = ProviderType::OpenAi;
            if provider.base_url.is_none() {
                provider.base_url = Some(String::from("https://api.openai.com/v1"));
            }
            if provider.env_var_api_key.is_none() {
                provider.env_var_api_key = Some(String::from("OPENAI_API_KEY"));
            }
        }
    }

    fn current_provider(&self) -> Option<&ProviderSettings> {
        self.state
            .settings_draft
            .as_ref()
            .and_then(|draft| draft.providers.get(self.state.settings_provider_index))
    }

    fn current_provider_fields(&self) -> Vec<SettingsProviderField> {
        match self
            .current_provider()
            .map(|provider| &provider.provider_type)
        {
            Some(ProviderType::OpenAi) => vec![
                SettingsProviderField::Id,
                SettingsProviderField::Type,
                SettingsProviderField::BaseUrl,
                SettingsProviderField::ApiKey,
                SettingsProviderField::EnvVarApiKey,
                SettingsProviderField::OnlyListedModels,
            ],
            None => Vec::new(),
        }
    }

    fn current_provider_field(&self) -> Option<SettingsProviderField> {
        self.current_provider_fields()
            .get(self.state.settings_provider_field_index)
            .copied()
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

    fn current_model_field(&self) -> Option<SettingsModelField> {
        SETTINGS_MODEL_FIELDS
            .get(self.state.settings_model_field_index)
            .copied()
    }

    fn handle_submit(&mut self) {
        let raw_input = self.state.input.trim().to_string();
        if raw_input.is_empty() {
            return;
        }
        self.state.input.clear();
        self.state.input_cursor = 0;
        let command_popup_dismissed = self.state.command_popup_dismissed;
        self.state.command_popup_dismissed = false;

        if !command_popup_dismissed
            && let Some(command) = raw_input.strip_prefix('/')
            && is_known_slash_command(command)
        {
            self.handle_command(command.trim());
            return;
        }

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

        if let Some(session_id) = self.state.current_session_id.clone() {
            self.dispatch_send_message(session_id, raw_input, model_id, provider_id);
            return;
        }

        self.state.pending_submit = Some(PendingSubmit {
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
            "settings" => {
                self.request(RuntimeRequest::GetSettings);
            }
            "sessions" => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::SessionsMenu;
                self.request(RuntimeRequest::ListSessions);
            }
            "tools" => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::ToolsMenu;
            }
            "new" => {
                self.start_new_chat_draft();
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

    fn flatten_models(&self) -> Vec<(String, Model)> {
        flatten_models_map(&self.state.models_by_provider)
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
        if let Some(tool) = self.state.pending_tools.iter_mut().find(|tool| {
            tool.call_id == call_id
                && self.state.current_session_id.as_deref() == Some(tool.session_id.as_str())
        }) {
            tool.approved = approved;
        }
    }

    fn current_session_pending_tools(&self) -> Vec<&PendingTool> {
        self.state
            .pending_tools
            .iter()
            .filter(|tool| {
                self.state.current_session_id.as_deref() == Some(tool.session_id.as_str())
            })
            .collect()
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
    }

    fn reset_chat_session(&mut self, session_id: Option<String>, status: &str) {
        self.state.mode = UiMode::Chat;
        self.state.current_session_id = session_id;
        self.state.current_tip_id = None;
        self.state.chat_history.clear();
        self.state.optimistic_messages.clear();
        self.state.is_streaming = false;
        self.state.auto_scroll = true;
        self.state.scroll = 0;
        self.state.status = status.to_string();
        self.invalidate_chat_cache();
        self.clamp_chat_scroll();
    }

    fn start_new_chat_draft(&mut self) {
        self.state.pending_submit = None;
        self.reset_chat_session(None, "Started new chat");
    }

    fn dispatch_send_message(
        &mut self,
        session_id: String,
        message: String,
        model_id: String,
        provider_id: String,
    ) {
        self.state.optimistic_seq = self.state.optimistic_seq.saturating_add(1);
        self.state.optimistic_messages.push(OptimisticMessage {
            local_id: format!("local-user-{}", self.state.optimistic_seq),
            content: message.clone(),
        });

        self.state.is_streaming = true;
        self.state.status = format!("Sending with {provider_id}/{model_id}");
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
            let key = optimistic.content.trim().to_string();
            match seen_users.get_mut(&key) {
                Some(count) if *count > 0 => {
                    *count -= 1;
                    false
                }
                _ => true,
            }
        });

        if self.state.optimistic_messages.len() != before_len {
            self.invalidate_chat_cache();
        }
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

    fn session_waiting_for_approval(&self, session_id: &str) -> bool {
        self.pending_tools
            .iter()
            .any(|tool| tool.session_id == session_id && tool.approved.is_none())
    }

    fn rendered_messages(&self) -> Vec<Message> {
        let mut rendered_messages: Vec<Message> =
            build_tip_chain(&self.chat_history, self.current_tip_id.as_deref())
                .into_iter()
                .cloned()
                .collect();

        for optimistic in &self.optimistic_messages {
            rendered_messages.push(Message {
                id: MessageId::new(optimistic.local_id.clone()),
                parent_id: None,
                role: ChatRole::User,
                content: optimistic.content.clone(),
                status: MessageStatus::Complete,
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
        Event, Model, ModelSettings, ProviderSettings, ProviderType, Session, SettingsDocument,
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
        PendingSubmit, PendingTool, RuntimeRequest, RuntimeResponse, SettingsFocus,
        SettingsModelField, SettingsProviderField, UiMode, is_copy_shortcut, menu_scroll_offset,
        model_menu_next_index, model_menu_previous_index, render_chat_selection_overlay,
        selection_text,
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
        }
    }

    fn sample_models() -> HashMap<String, Vec<Model>> {
        HashMap::from([(
            String::from("openai"),
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
                id: String::from("openai"),
                provider_type: ProviderType::OpenAi,
                base_url: Some(String::from("https://api.openai.com/v1")),
                api_key: None,
                env_var_api_key: Some(String::from("OPENAI_API_KEY")),
                only_listed_models: true,
            }],
            models: vec![ModelSettings {
                id: String::from("gpt-4o-mini"),
                provider_id: String::from("openai"),
                name: Some(String::from("GPT-4o Mini")),
                max_context: Some(128_000),
            }],
        }
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
            },
            Session {
                id: String::from("sess-2"),
                tip_id: Some(String::from("m3")),
                workspace_dir: String::from("/tmp/project-b"),
                created_at: 3,
                updated_at: 4,
                title: Some(String::from("Testing plan")),
            },
        ]
    }

    fn sample_pending_tools() -> Vec<PendingTool> {
        vec![
            PendingTool {
                session_id: String::from("sess-2"),
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
            },
            PendingTool {
                session_id: String::from("sess-2"),
                call_id: String::from("call-2"),
                tool_id: String::from("write_file"),
                args: String::from("{\"path\":\"src/app.rs\",\"content\":\"...\"}"),
                description: String::from("Patch the app module"),
                risk_level: String::from("undoable_workspace_write"),
                reasons: vec![String::from("Updates tracked source code")],
                approved: None,
            },
        ]
    }

    fn populated_state() -> AppState {
        AppState {
            config_loaded: true,
            status: String::from("Ready"),
            models_by_provider: sample_models(),
            selected_provider_id: Some(String::from("openai")),
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
14: Ready | model=openai/gpt-4o-mini | tools=2 | idle
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
11:  ┌Command (Tab/Down next, Shift-Tab/Up prev┐
12:  │> /settings  Open settings editor        │
13:  │  /sessions  Open sessions menu          │
14: R└─────────────────────────────────────────┘ idle
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
06:          │  openai / GPT-4.1 Mini                             │
07:          │> openai / GPT-4o Mini (current)                    │
08:          │                                                    │
09:          │                                                    │
10:          │                                                    │
11:          │                                                    │
12:          └────────────────────────────────────────────────────┘
14: Ready | model=openai/gpt-4o-mini | tools=2 | idle
16:  >"#,
        );
    }

    #[test]
    fn renders_settings_menu_snapshot() {
        let mut state = populated_state();
        state.mode = UiMode::SettingsMenu;
        state.settings_focus = SettingsFocus::ProviderForm;
        state.settings_provider_field_index = 2;
        state.settings_model_field_index = 2;
        state.settings_errors = HashMap::from([
            (
                String::from("providers[0].base_url"),
                String::from("must be a valid URL"),
            ),
            (
                String::from("models[0].max_context"),
                String::from("must be a positive integer"),
            ),
        ]);

        let rendered = render_state_snapshot(&state, 100, 22);

        assert_snapshot(
            &rendered,
            r#"01: How should we test the TUI?
02:      ┌/settings───────────────────────────────────────────────────────────────────────────────┐
03:      │Settings Editor                                                                         │
04: Use r│Tab=next pane, Enter=edit/toggle, a=add, x=delete, s=save, Esc=close                    │
05:      │┌───────────────────┐┌─────────────────────┐┌───────────────────┐┌─────────────────────┐│
06:      ││Providers          ││Provider Fields      ││Models             ││Model Fields         ││
07:      ││> openai           ││  Provider ID        ││> gpt-4o-mini      ││  Model ID           ││
08:      ││                   ││  Provider Type      ││                   ││  Display Name       ││
09:      ││                   ││> Base URL           ││                   ││> Max Context        ││
10:      ││                   ││  Inline API Key     ││                   ││                     ││
11:      ││                   ││  Env Var            ││                   ││                     ││
12:      ││                   ││  Only Listed Models ││                   ││                     ││
13:      ││                   ││                     ││                   ││                     ││
14:      │└───────────────────┘└─────────────────────┘└───────────────────┘└─────────────────────┘│
15:      │┌──────────────────────────────────────────────────────────────────────────────────────┐│
16:      ││Providers and models are shared with the desktop app                                  ││
17:      │└──────────────────────────────────────────────────────────────────────────────────────┘│
18: Ready└────────────────────────────────────────────────────────────────────────────────────────┘
20:  >"#,
        );
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
07:        │  Refactor ideas                                       │
08:        │> Testing plan (current) [approval]                    │
09:        │                                                       │
10:        │                                                       │
11:        │                                                       │
12:        └───────────────────────────────────────────────────────┘
14: Ready | model=openai/gpt-4o-mini | tools=2 | idle
16:  >"#,
        );
    }

    #[test]
    fn renders_tools_menu_snapshot() {
        let mut state = populated_state();
        state.mode = UiMode::ToolsMenu;
        state.tools_menu_index = 1;

        let rendered = render_state_snapshot(&state, 80, 20);

        assert_snapshot(
            &rendered,
            r#"01: How should we test the TUI?
03:         ┌/tools────────────────────────────────────────────────────────┐
04: Use rend│Tools (a=approve, d=deny, e=execute approved, Esc=close)      │oke test
05: s.      │  [approved] read_file - Inspect the app module               │
06:         │> [pending] write_file - Patch the app module                 │
07:         │    args: {"path":"src/app.rs","content":"..."}               │
08:         │    risk: undoable_workspace_write                            │
09:         │    why: Updates tracked source code                          │
10:         │                                                              │
11:         │                                                              │
12:         │                                                              │
13:         │                                                              │
14:         │                                                              │
15:         └──────────────────────────────────────────────────────────────┘
16: Ready | model=openai/gpt-4o-mini | tools=2 | idle
18:  >"#,
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
06:               │/help      Open this help menu           │
07:               │/model     Open model selector           │
08:               │/settings  Open settings editor          │
09:               │/sessions  Open sessions menu            │
10:               │/tools     Open tools approval menu      │
11:               │/new       Start a new session           │
12:               └─────────────────────────────────────────┘
14: Ready | model=openai/gpt-4o-mini | tools=2 | idle
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
    fn new_command_starts_local_draft_without_creating_session() {
        let mut harness = test_harness();
        harness.app.state = populated_state();
        harness.app.state.pending_submit = Some(PendingSubmit {
            message: String::from("stale"),
            model_id: String::from("gpt-4o-mini"),
            provider_id: String::from("openai"),
        });

        harness.app.handle_command("new");

        assert_eq!(harness.app.state.mode, UiMode::Chat);
        assert_eq!(harness.app.state.current_session_id, None);
        assert_eq!(harness.app.state.current_tip_id, None);
        assert!(harness.app.state.chat_history.is_empty());
        assert!(harness.app.state.optimistic_messages.is_empty());
        assert!(harness.app.state.pending_submit.is_none());
        assert_eq!(harness.app.state.status, "Started new chat");
        assert!(harness.drain_requests().is_empty());
    }

    #[test]
    fn first_submit_after_new_command_creates_session_lazily() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.selected_provider_id = Some(String::from("openai"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));

        harness.app.handle_command("new");
        harness.app.state.input = String::from("hello world");
        harness.app.state.input_cursor = harness.app.state.input.len();
        harness.app.handle_key_event(key(KeyCode::Enter));

        assert_eq!(
            harness
                .app
                .state
                .pending_submit
                .as_ref()
                .map(|submit| submit.message.as_str()),
            Some("hello world")
        );
        assert_eq!(harness.app.state.status, "Creating session");

        let requests = harness.drain_requests();
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], RuntimeRequest::CreateSession));
    }

    #[test]
    fn sessions_menu_new_chat_starts_local_draft_without_creating_session() {
        let mut harness = test_harness();
        harness.app.state = populated_state();
        harness.app.state.mode = UiMode::SessionsMenu;
        harness.app.state.sessions_menu_index = 0;

        harness
            .app
            .handle_sessions_menu_key_event(key(KeyCode::Enter));

        assert_eq!(harness.app.state.mode, UiMode::Chat);
        assert_eq!(harness.app.state.current_session_id, None);
        assert!(harness.app.state.chat_history.is_empty());
        assert_eq!(harness.app.state.status, "Started new chat");
        assert!(harness.drain_requests().is_empty());
    }

    #[test]
    fn submit_sends_message_request_and_tracks_optimistic_message() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.selected_provider_id = Some(String::from("openai"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
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
                assert_eq!(provider_id, "openai");
            }
            other => panic!("unexpected request: {}", request_name(other)),
        }
    }

    #[test]
    fn submit_unknown_slash_command_sends_message() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.selected_provider_id = Some(String::from("openai"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
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
                assert_eq!(provider_id, "openai");
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
    fn escape_closes_settings_editor_before_closing_settings_menu() {
        let mut harness = test_harness();
        harness.app.state.mode = UiMode::SettingsMenu;
        harness.app.state.settings_draft = Some(sample_settings());
        harness.app.state.settings_editor =
            Some(ActiveSettingsEditor::Provider(SettingsProviderField::Id));
        harness.app.state.settings_editor_input = String::from("draft-openai");

        harness.app.handle_key_event(key(KeyCode::Esc));
        assert_eq!(harness.app.state.mode, UiMode::SettingsMenu);
        assert_eq!(harness.app.state.settings_editor, None);
        assert!(harness.app.state.settings_editor_input.is_empty());

        harness.app.handle_key_event(key(KeyCode::Esc));
        assert_eq!(harness.app.state.mode, UiMode::Chat);
        assert_eq!(harness.app.state.status, "Settings editor closed");
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
    fn escape_then_typing_partial_command_submits_message() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.selected_provider_id = Some(String::from("openai"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
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
                assert_eq!(provider_id, "openai");
            }
            other => panic!("unexpected request: {}", request_name(other)),
        }
    }

    #[test]
    fn escape_then_typing_full_command_submits_message() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.selected_provider_id = Some(String::from("openai"));
        harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
        harness.app.state.input = String::from("/s");
        harness.app.state.input_cursor = harness.app.state.input.len();

        harness.app.handle_key_event(key(KeyCode::Esc));
        for ch in "ettings".chars() {
            harness.app.handle_key_event(key(KeyCode::Char(ch)));
        }
        harness.app.handle_key_event(key(KeyCode::Enter));

        assert_eq!(harness.app.state.mode, UiMode::Chat);
        assert!(harness.app.state.is_streaming);
        assert_eq!(harness.app.state.optimistic_messages.len(), 1);
        assert_eq!(
            harness.app.state.optimistic_messages[0].content,
            "/settings"
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
                assert_eq!(message, "/settings");
                assert_eq!(model_id, "gpt-4o-mini");
                assert_eq!(provider_id, "openai");
            }
            other => panic!("unexpected request: {}", request_name(other)),
        }
    }

    #[test]
    fn tool_menu_execute_approved_returns_to_chat_and_requests_execution() {
        let mut harness = test_harness();
        harness.app.state.mode = UiMode::ToolsMenu;
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.pending_tools = sample_pending_tools();

        harness.app.handle_key_event(key(KeyCode::Char('e')));

        assert_eq!(harness.app.state.mode, UiMode::Chat);
        let requests = harness.drain_requests();
        assert_eq!(requests.len(), 1);
        assert!(matches!(
            &requests[0],
            RuntimeRequest::ExecuteApprovedTools { session_id } if session_id == "sess-2"
        ));
    }

    #[test]
    fn settings_save_error_populates_field_errors() {
        let mut harness = test_harness();
        harness.app.state.mode = UiMode::SettingsMenu;
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
        harness
            .app
            .state
            .optimistic_messages
            .push(OptimisticMessage {
                local_id: String::from("local-user-1"),
                content: String::from("hello world"),
            });

        harness
            .app
            .handle_runtime_response(RuntimeResponse::ChatHistory(Ok(BTreeMap::from([(
                MessageId::new("m1"),
                message("m1", None, ChatRole::User, "hello world"),
            )]))));

        assert!(harness.app.state.optimistic_messages.is_empty());
        assert_eq!(harness.app.state.chat_history.len(), 1);
    }

    #[test]
    fn settings_response_opens_editor_with_clean_state() {
        let mut harness = test_harness();
        harness.app.state.settings_errors =
            HashMap::from([(String::from("providers[0].id"), String::from("old error"))]);
        harness.app.state.settings_editor =
            Some(ActiveSettingsEditor::Model(SettingsModelField::Name));
        harness.app.state.settings_editor_input = String::from("stale");
        harness.app.state.settings_delete_armed = true;

        harness
            .app
            .handle_runtime_response(RuntimeResponse::Settings(Ok(sample_settings())));

        assert_eq!(harness.app.state.mode, UiMode::SettingsMenu);
        assert_eq!(harness.app.state.status, "Editing settings");
        assert!(harness.app.state.settings_errors.is_empty());
        assert_eq!(harness.app.state.settings_editor, None);
        assert!(harness.app.state.settings_editor_input.is_empty());
        assert!(!harness.app.state.settings_delete_armed);
    }

    #[test]
    fn approve_tool_response_marks_pending_tool_as_approved() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-2"));
        harness.app.state.pending_tools = sample_pending_tools();

        harness
            .app
            .handle_runtime_response(RuntimeResponse::ApproveTool {
                call_id: String::from("call-2"),
                result: Ok(()),
            });

        assert_eq!(harness.app.state.pending_tools[1].approved, Some(true));
    }

    #[test]
    fn background_session_pending_tool_survives_session_switch() {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("sess-1"));

        harness.app.handle_runtime_event(Event::ToolCallDetected {
            session_id: String::from("sess-1"),
            call_id: String::from("call-bg"),
            tool_id: String::from("read_file"),
            args: String::from("{\"path\":\"Cargo.toml\"}"),
            description: String::from("Read the workspace manifest"),
            risk_level: String::from("read_only_workspace"),
            reasons: vec![String::from("Reads local config")],
        });
        harness
            .app
            .handle_runtime_response(RuntimeResponse::LoadSession {
                session_id: String::from("sess-2"),
                result: Ok(true),
            });

        assert_eq!(harness.app.state.current_session_id.as_deref(), Some("sess-2"));
        assert_eq!(harness.app.state.pending_tools.len(), 1);
        assert_eq!(harness.app.state.pending_tools[0].session_id, "sess-1");
        assert!(harness.app.state.session_waiting_for_approval("sess-1"));
        assert!(!harness.app.state.session_waiting_for_approval("sess-2"));
    }

    #[test]
    fn session_waiting_for_approval_ignores_handled_tools() {
        let state = AppState {
            pending_tools: vec![
                PendingTool {
                    session_id: String::from("sess-1"),
                    call_id: String::from("approved"),
                    tool_id: String::from("read_file"),
                    args: String::from("{}"),
                    description: String::from("Approved tool"),
                    risk_level: String::from("read_only_workspace"),
                    reasons: Vec::new(),
                    approved: Some(true),
                },
                PendingTool {
                    session_id: String::from("sess-1"),
                    call_id: String::from("denied"),
                    tool_id: String::from("write_file"),
                    args: String::from("{}"),
                    description: String::from("Denied tool"),
                    risk_level: String::from("undoable_workspace_write"),
                    reasons: Vec::new(),
                    approved: Some(false),
                },
            ],
            ..AppState::default()
        };

        assert!(!state.session_waiting_for_approval("sess-1"));
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
        });

        assert_eq!(harness.app.state.pending_tools.len(), 1);
        assert_eq!(
            harness.app.state.status,
            "1 tool call(s) pending. Use /tools"
        );
    }

    fn request_name(request: &RuntimeRequest) -> &'static str {
        match request {
            RuntimeRequest::ListModels => "ListModels",
            RuntimeRequest::GetSettings => "GetSettings",
            RuntimeRequest::CreateSession => "CreateSession",
            RuntimeRequest::SendMessage { .. } => "SendMessage",
            RuntimeRequest::SaveSettings { .. } => "SaveSettings",
            RuntimeRequest::GetChatHistory { .. } => "GetChatHistory",
            RuntimeRequest::GetCurrentTip { .. } => "GetCurrentTip",
            RuntimeRequest::LoadSession { .. } => "LoadSession",
            RuntimeRequest::ListSessions => "ListSessions",
            RuntimeRequest::DeleteSession { .. } => "DeleteSession",
            RuntimeRequest::ApproveTool { .. } => "ApproveTool",
            RuntimeRequest::DenyTool { .. } => "DenyTool",
            RuntimeRequest::ExecuteApprovedTools { .. } => "ExecuteApprovedTools",
        }
    }
}
