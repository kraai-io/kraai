use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_runtime::{
    Event, Model, ModelSettings, ProviderSettings, ProviderType, RuntimeHandle, Session,
    SettingsDocument,
};
use base64::Engine;
use color_eyre::eyre::Result;
use crossbeam_channel::{Receiver, Sender, unbounded};
use ratatui::{
    buffer::Buffer,
    crossterm::event::{
        self, Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use types::{ChatRole, Message, MessageId, MessageStatus};

use crate::components::{ChatHistory, RenderedLine, TextInput, VisibleChatView};

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
    SendMessage {
        message: String,
        model_id: String,
        provider_id: String,
    },
    SaveSettings {
        settings: SettingsDocument,
    },
    GetChatHistory,
    GetCurrentTip,
    ClearCurrentSession,
    LoadSession {
        session_id: String,
    },
    ListSessions,
    DeleteSession {
        session_id: String,
    },
    GetCurrentSessionId,
    ApproveTool {
        call_id: String,
    },
    DenyTool {
        call_id: String,
    },
    ExecuteApprovedTools,
}

enum RuntimeResponse {
    Models(Result<HashMap<String, Vec<Model>>, String>),
    Settings(Result<SettingsDocument, String>),
    SendMessage(Result<(), String>),
    SaveSettings(Result<(), String>),
    ChatHistory(Result<BTreeMap<MessageId, Message>, String>),
    CurrentTip(Result<Option<String>, String>),
    ClearCurrentSession(Result<(), String>),
    LoadSession {
        session_id: String,
        result: Result<bool, String>,
    },
    Sessions(Result<Vec<Session>, String>),
    DeleteSession {
        session_id: String,
        result: Result<(), String>,
    },
    CurrentSessionId(Result<Option<String>, String>),
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
    chat_history: BTreeMap<MessageId, Message>,
    optimistic_messages: Vec<OptimisticMessage>,
    optimistic_seq: u64,
    chat_epoch: u64,
    chat_render_cache: RefCell<ChatRenderCache>,
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
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            exit: false,
            input: String::new(),
            input_cursor: 0,
            chat_history: BTreeMap::new(),
            optimistic_messages: Vec::new(),
            optimistic_seq: 0,
            chat_epoch: 0,
            chat_render_cache: RefCell::new(ChatRenderCache::default()),
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
    pub fn new(runtime: RuntimeHandle, event_rx: Receiver<Event>) -> Self {
        let (runtime_tx, req_rx): (Sender<RuntimeRequest>, Receiver<RuntimeRequest>) = unbounded();
        let (res_tx, runtime_rx): (Sender<RuntimeResponse>, Receiver<RuntimeResponse>) =
            unbounded();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");

            while let Ok(req) = req_rx.recv() {
                match req {
                    RuntimeRequest::ListModels => {
                        let result = rt
                            .block_on(runtime.list_models())
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::Models(result));
                    }
                    RuntimeRequest::GetSettings => {
                        let result = rt
                            .block_on(runtime.get_settings())
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::Settings(result));
                    }
                    RuntimeRequest::SendMessage {
                        message,
                        model_id,
                        provider_id,
                    } => {
                        let result = rt
                            .block_on(runtime.send_message(message, model_id, provider_id))
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::SendMessage(result));
                    }
                    RuntimeRequest::SaveSettings { settings } => {
                        let result = rt
                            .block_on(runtime.save_settings(settings))
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::SaveSettings(result));
                    }
                    RuntimeRequest::GetChatHistory => {
                        let result = rt
                            .block_on(runtime.get_chat_history())
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::ChatHistory(result));
                    }
                    RuntimeRequest::GetCurrentTip => {
                        let result = rt
                            .block_on(runtime.get_current_tip())
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::CurrentTip(result));
                    }
                    RuntimeRequest::ClearCurrentSession => {
                        let result = rt
                            .block_on(runtime.clear_current_session())
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::ClearCurrentSession(result));
                    }
                    RuntimeRequest::LoadSession { session_id } => {
                        let result = rt
                            .block_on(runtime.load_session(session_id.clone()))
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::LoadSession { session_id, result });
                    }
                    RuntimeRequest::ListSessions => {
                        let result = rt
                            .block_on(runtime.list_sessions())
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::Sessions(result));
                    }
                    RuntimeRequest::DeleteSession { session_id } => {
                        let result = rt
                            .block_on(runtime.delete_session(session_id.clone()))
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::DeleteSession { session_id, result });
                    }
                    RuntimeRequest::GetCurrentSessionId => {
                        let result = rt
                            .block_on(runtime.get_current_session_id())
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::CurrentSessionId(result));
                    }
                    RuntimeRequest::ApproveTool { call_id } => {
                        let result = rt
                            .block_on(runtime.approve_tool(call_id.clone()))
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::ApproveTool { call_id, result });
                    }
                    RuntimeRequest::DenyTool { call_id } => {
                        let result = rt
                            .block_on(runtime.deny_tool(call_id.clone()))
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::DenyTool { call_id, result });
                    }
                    RuntimeRequest::ExecuteApprovedTools => {
                        let result = rt
                            .block_on(runtime.execute_approved_tools())
                            .map_err(|e| e.to_string());
                        let _ = res_tx.send(RuntimeResponse::ExecuteApprovedTools(result));
                    }
                }
            }
        });

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
            Event::StreamStart { .. } => {
                self.state.is_streaming = true;
                self.last_stream_refresh = None;
                self.request(RuntimeRequest::GetCurrentTip);
            }
            Event::StreamChunk { .. } => {
                let now = Instant::now();
                let should_refresh = self
                    .last_stream_refresh
                    .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(50));
                if should_refresh {
                    self.last_stream_refresh = Some(now);
                    self.request(RuntimeRequest::GetCurrentTip);
                    self.request(RuntimeRequest::GetChatHistory);
                }
            }
            Event::StreamComplete { .. } => {
                self.state.is_streaming = false;
                self.last_stream_refresh = None;
                self.request_sync();
            }
            Event::StreamError { error, .. } => {
                self.state.is_streaming = false;
                self.last_stream_refresh = None;
                self.state.status = format!("Stream error: {error}");
            }
            Event::HistoryUpdated => {
                self.request_sync();
            }
            Event::MessageComplete(_) => {}
            Event::ToolCallDetected {
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
                        self.state.pending_tools.len()
                    );
                }
            }
            Event::ToolResultReady {
                call_id,
                tool_id,
                success,
                denied,
                ..
            } => {
                self.state
                    .pending_tools
                    .retain(|tool| tool.call_id != call_id);
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
            }
            RuntimeResponse::ChatHistory(Err(err)) => {
                self.state.status = format!("Failed loading history: {err}");
            }
            RuntimeResponse::CurrentTip(Ok(tip)) => {
                if self.state.current_tip_id != tip {
                    self.state.current_tip_id = tip;
                    self.invalidate_chat_cache();
                    self.reconcile_optimistic_messages();
                }
            }
            RuntimeResponse::CurrentTip(Err(err)) => {
                self.state.status = format!("Failed loading tip: {err}");
            }
            RuntimeResponse::ClearCurrentSession(Ok(())) => {
                self.state.status = String::from("Started new session");
                self.state.chat_history.clear();
                self.state.optimistic_messages.clear();
                self.invalidate_chat_cache();
                self.request_sync();
            }
            RuntimeResponse::ClearCurrentSession(Err(err)) => {
                self.state.status = format!("Failed to clear session: {err}");
            }
            RuntimeResponse::LoadSession {
                session_id,
                result: Ok(true),
            } => {
                self.state.mode = UiMode::Chat;
                self.state.current_session_id = Some(session_id);
                self.state.current_tip_id = None;
                self.state.chat_history.clear();
                self.state.optimistic_messages.clear();
                self.state.is_streaming = false;
                self.state.auto_scroll = true;
                self.state.scroll = 0;
                self.state.status = String::from("Session loaded");
                self.invalidate_chat_cache();
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
                    self.state.current_session_id = None;
                    self.state.chat_history.clear();
                    self.invalidate_chat_cache();
                }
            }
            RuntimeResponse::DeleteSession {
                result: Err(err), ..
            } => {
                self.state.status = format!("Failed deleting session: {err}");
            }
            RuntimeResponse::CurrentSessionId(Ok(current)) => {
                self.state.current_session_id = current;
            }
            RuntimeResponse::CurrentSessionId(Err(err)) => {
                self.state.status = format!("Failed loading current session: {err}");
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
            match event::read()? {
                CrosstermEvent::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                    self.handle_key_event(key_event);
                    changed = true;
                }
                CrosstermEvent::Mouse(mouse_event) => {
                    self.handle_mouse_event(mouse_event);
                    changed = true;
                }
                CrosstermEvent::Resize(_, _) => {
                    self.clear_chat_selection();
                    self.state.visible_chat_view = None;
                    changed = true;
                }
                _ => {}
            }

            if !event::poll(std::time::Duration::from_millis(0))? {
                break;
            }
        }

        Ok(changed)
    }

    fn handle_mouse_event(&mut self, mouse_event: MouseEvent) {
        if self.state.mode != UiMode::Chat {
            return;
        }

        match mouse_event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(position) = self.hit_test_chat_cell(mouse_event.column, mouse_event.row) {
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
                self.state.auto_scroll = false;
                self.state.scroll = self.state.scroll.saturating_sub(3);
                self.clear_chat_selection();
            }
            MouseEventKind::ScrollDown => {
                self.state.auto_scroll = false;
                self.state.scroll = self.state.scroll.saturating_add(3);
                self.clear_chat_selection();
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
                self.state.auto_scroll = false;
                self.state.scroll = self.state.scroll.saturating_sub(10);
                self.clear_chat_selection();
            }
            KeyCode::PageDown => {
                self.state.auto_scroll = false;
                self.state.scroll = self.state.scroll.saturating_add(10);
                self.clear_chat_selection();
            }
            KeyCode::Home => {
                self.state.auto_scroll = false;
                self.state.scroll = 0;
                self.clear_chat_selection();
            }
            KeyCode::End => {
                self.state.auto_scroll = true;
                self.clear_chat_selection();
            }
            _ => {}
        }
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
                    self.request(RuntimeRequest::ClearCurrentSession);
                    self.state.mode = UiMode::Chat;
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
        let len = self.state.pending_tools.len();

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
                if let Some(tool) = self.state.pending_tools.get(self.state.tools_menu_index) {
                    self.request(RuntimeRequest::ApproveTool {
                        call_id: tool.call_id.clone(),
                    });
                }
            }
            KeyCode::Char('d') => {
                if let Some(tool) = self.state.pending_tools.get(self.state.tools_menu_index) {
                    self.request(RuntimeRequest::DenyTool {
                        call_id: tool.call_id.clone(),
                    });
                }
            }
            KeyCode::Char('e') => {
                if self
                    .state
                    .pending_tools
                    .iter()
                    .any(|tool| tool.approved == Some(true))
                {
                    self.request(RuntimeRequest::ExecuteApprovedTools);
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
        let provider_index = self.state.settings_provider_index;
        if let Some(provider) = self
            .state
            .settings_draft
            .as_mut()
            .and_then(|draft| draft.providers.get_mut(provider_index))
        {
            provider.provider_type = match (provider.provider_type.clone(), forward) {
                (ProviderType::OpenAi, true) => ProviderType::Google,
                (ProviderType::Google, true) => ProviderType::OpenAi,
                (ProviderType::OpenAi, false) => ProviderType::Google,
                (ProviderType::Google, false) => ProviderType::OpenAi,
            };

            match provider.provider_type {
                ProviderType::OpenAi => {
                    if provider.base_url.is_none() {
                        provider.base_url = Some(String::from("https://api.openai.com/v1"));
                    }
                    if provider.env_var_api_key.as_deref() == Some("GEMINI_API_KEY")
                        || provider.env_var_api_key.is_none()
                    {
                        provider.env_var_api_key = Some(String::from("OPENAI_API_KEY"));
                    }
                }
                ProviderType::Google => {
                    provider.base_url = None;
                    if provider.env_var_api_key.as_deref() == Some("OPENAI_API_KEY")
                        || provider.env_var_api_key.is_none()
                    {
                        provider.env_var_api_key = Some(String::from("GEMINI_API_KEY"));
                    }
                }
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
            Some(ProviderType::Google) => vec![
                SettingsProviderField::Id,
                SettingsProviderField::Type,
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

        self.state.optimistic_seq = self.state.optimistic_seq.saturating_add(1);
        self.state.optimistic_messages.push(OptimisticMessage {
            local_id: format!("local-user-{}", self.state.optimistic_seq),
            content: raw_input.clone(),
        });

        self.state.is_streaming = true;
        self.state.status = format!("Sending with {provider_id}/{model_id}");
        self.state.auto_scroll = true;
        self.state.current_tip_id = None;
        self.invalidate_chat_cache();

        self.request(RuntimeRequest::SendMessage {
            message: raw_input,
            model_id,
            provider_id,
        });
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
                self.request(RuntimeRequest::GetCurrentSessionId);
            }
            "tools" => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::ToolsMenu;
            }
            "new" => {
                self.request(RuntimeRequest::ClearCurrentSession);
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
                arboard::Clipboard::new()
                    .map_err(|err| format!("clipboard unavailable: {err}"))?,
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

    fn request_sync(&self) {
        self.request(RuntimeRequest::ListModels);
        self.request(RuntimeRequest::ListSessions);
        self.request(RuntimeRequest::GetCurrentSessionId);
        self.request(RuntimeRequest::GetCurrentTip);
        self.request(RuntimeRequest::GetChatHistory);
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
    fn refresh_chat_render_cache(&self, width: u16) {
        let needs_refresh = {
            let cache = self.chat_render_cache.borrow();
            cache.epoch != self.chat_epoch || cache.width != width
        };
        if !needs_refresh {
            return;
        }

        let rendered_messages = self.rendered_messages();
        let mut cache = self.chat_render_cache.borrow_mut();
        let mut prior_entries = std::mem::take(&mut cache.message_cache);
        if cache.width != width {
            prior_entries.clear();
        }

        let mut next_entries: HashMap<String, CachedMessageRender> = HashMap::new();
        let mut sections: Vec<Arc<Vec<RenderedLine>>> = Vec::new();
        let mut total_lines: u16 = 0;

        for msg in &rendered_messages {
            let key = msg.id.as_str().to_string();
            let fingerprint = message_fingerprint(msg);
            let lines = match prior_entries.remove(&key) {
                Some(entry) if entry.fingerprint == fingerprint => entry.lines,
                _ => Arc::new(ChatHistory::build_message_lines(msg, width)),
            };

            if lines.is_empty() {
                continue;
            }

            if !sections.is_empty() {
                sections.push(Arc::new(vec![ChatHistory::separator_line()]));
                total_lines = total_lines.saturating_add(1);
            }

            total_lines = total_lines.saturating_add(lines.len().min(u16::MAX as usize) as u16);
            sections.push(Arc::clone(&lines));
            next_entries.insert(key, CachedMessageRender { fingerprint, lines });
        }

        cache.sections = sections;
        cache.total_lines = total_lines;
        cache.message_cache = next_entries;
        cache.width = width;
        cache.epoch = self.chat_epoch;
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

impl Widget for &AppState {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        let input_height = TextInput::new(&self.input, self.input_cursor).get_height(area.width);
        let layout = Layout::vertical([
            Constraint::Min(area.height.saturating_sub(input_height + 1)),
            Constraint::Length(1),
            Constraint::Length(input_height),
        ])
        .flex(Flex::End);
        let [chat_history_area, status_area, input_area] = layout.areas(area);

        self.refresh_chat_render_cache(chat_history_area.width);
        {
            let cache = self.chat_render_cache.borrow();
            ChatHistory::render_prebuilt_sections(
                &cache.sections,
                cache.total_lines,
                chat_history_area,
                buf,
                self.scroll,
                self.auto_scroll,
            );
        }
        render_chat_selection_overlay(self.visible_chat_view.as_ref(), self.selection, buf);

        let selected_model = self
            .selected_provider_id
            .as_ref()
            .zip(self.selected_model_id.as_ref())
            .map(|(p, m)| format!("{p}/{m}"))
            .unwrap_or_else(|| String::from("none"));
        let stream_state = if self.is_streaming {
            "streaming"
        } else {
            "idle"
        };
        let pending_tools = self.pending_tools.len();
        let status_line = format!(
            "{} | model={} | tools={} | {}",
            self.status, selected_model, pending_tools, stream_state
        );

        Paragraph::new(Line::raw(status_line))
            .style(Style::default().fg(Color::DarkGray))
            .render(status_area, buf);

        TextInput::new(&self.input, self.input_cursor).render(input_area, buf);
        if self.mode == UiMode::Chat {
            render_command_popup(self, area, input_area, buf);
        }

        match self.mode {
            UiMode::ModelMenu => render_model_menu(self, area, buf),
            UiMode::SettingsMenu => render_settings_menu(self, area, buf),
            UiMode::SessionsMenu => render_sessions_menu(self, area, buf),
            UiMode::ToolsMenu => render_tools_menu(self, area, buf),
            UiMode::Help => render_help_menu(area, buf),
            UiMode::Chat => {}
        }
    }
}

fn render_chat_selection_overlay(
    visible_chat_view: Option<&VisibleChatView>,
    selection: Option<ChatSelection>,
    buf: &mut Buffer,
) {
    let (Some(view), Some(selection)) = (visible_chat_view, selection) else {
        return;
    };
    let Some((start, end)) = normalized_selection_range(view, selection) else {
        return;
    };

    for line_index in start.line..=end.line {
        let Some(line) = view.lines.get(line_index) else {
            continue;
        };
        let line_width = line.text.chars().count();
        if line_width == 0 {
            continue;
        }

        let start_col = if line_index == start.line {
            start.column.min(line_width.saturating_sub(1))
        } else {
            0
        };
        let end_col = if line_index == end.line {
            end.column.min(line_width.saturating_sub(1))
        } else {
            line_width.saturating_sub(1)
        };

        for column in start_col..=end_col {
            let x = view.area.x + column as u16;
            let y = line.y;
            let cell = &mut buf[(x, y)];
            cell.set_fg(Color::Black);
            cell.set_bg(Color::Cyan);
        }
    }
}

fn normalized_selection_range(
    view: &VisibleChatView,
    selection: ChatSelection,
) -> Option<(ChatCellPosition, ChatCellPosition)> {
    let (mut start, mut end) = selection.normalized();
    let start_width = view.lines.get(start.line)?.text.chars().count();
    let end_width = view.lines.get(end.line)?.text.chars().count();

    if start_width > 0 {
        start.column = start.column.min(start_width.saturating_sub(1));
    } else {
        start.column = 0;
    }

    if end_width > 0 {
        end.column = end.column.min(end_width.saturating_sub(1));
    } else {
        end.column = 0;
    }

    Some((start, end))
}

fn selection_text(view: &VisibleChatView, selection: ChatSelection) -> Option<String> {
    let (start, end) = normalized_selection_range(view, selection)?;
    let mut selected_lines = Vec::new();

    for line_index in start.line..=end.line {
        let line = view.lines.get(line_index)?;
        let chars: Vec<char> = line.text.chars().collect();
        let line_width = chars.len();

        let text = if line_width == 0 {
            String::new()
        } else {
            let start_col = if line_index == start.line {
                start.column.min(line_width.saturating_sub(1))
            } else {
                0
            };
            let end_col = if line_index == end.line {
                end.column.min(line_width.saturating_sub(1))
            } else {
                line_width.saturating_sub(1)
            };
            chars[start_col..=end_col].iter().collect()
        };

        selected_lines.push(text);
    }

    Some(selected_lines.join("\n"))
}

fn is_copy_shortcut(key_event: KeyEvent) -> bool {
    match key_event.code {
        KeyCode::Char(c) => {
            key_event.modifiers.contains(KeyModifiers::CONTROL)
                && (key_event.modifiers.contains(KeyModifiers::SHIFT) || c.is_ascii_uppercase())
                && c.eq_ignore_ascii_case(&'c')
        }
        _ => false,
    }
}

fn copy_via_osc52(text: &str) -> Result<(), String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let sequence = format!("\x1b]52;c;{encoded}\x07");
    let mut stdout = std::io::stdout();
    stdout
        .write_all(sequence.as_bytes())
        .map_err(|err| format!("stdout write failed: {err}"))?;
    stdout
        .flush()
        .map_err(|err| format!("stdout flush failed: {err}"))
}

fn active_command_prefix(input: &str) -> Option<&str> {
    let cmd = input.strip_prefix('/')?;
    if cmd.chars().any(char::is_whitespace) {
        return None;
    }
    Some(cmd)
}

fn is_known_slash_command(command_line: &str) -> bool {
    command_line
        .split_whitespace()
        .next()
        .is_some_and(|command| SLASH_COMMANDS.iter().any(|(known, _)| *known == command))
}

fn slash_command_matches(prefix: &str) -> Vec<(&'static str, &'static str)> {
    SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|(command, _)| command.starts_with(prefix))
        .collect()
}

const SETTINGS_MODEL_FIELDS: [SettingsModelField; 3] = [
    SettingsModelField::Id,
    SettingsModelField::Name,
    SettingsModelField::MaxContext,
];

fn adjust_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }

    if delta.is_negative() {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        (current + delta as usize).min(len - 1)
    }
}

fn provider_type_label(provider_type: &ProviderType) -> &'static str {
    match provider_type {
        ProviderType::OpenAi => "OpenAI-compatible",
        ProviderType::Google => "Google",
    }
}

fn settings_provider_field_label(field: SettingsProviderField) -> &'static str {
    match field {
        SettingsProviderField::Id => "Provider ID",
        SettingsProviderField::Type => "Provider Type",
        SettingsProviderField::BaseUrl => "Base URL",
        SettingsProviderField::ApiKey => "Inline API Key",
        SettingsProviderField::EnvVarApiKey => "Env Var",
        SettingsProviderField::OnlyListedModels => "Only Listed Models",
    }
}

fn settings_provider_field_value(
    provider: &ProviderSettings,
    field: SettingsProviderField,
) -> String {
    match field {
        SettingsProviderField::Id => provider.id.clone(),
        SettingsProviderField::Type => String::from(provider_type_label(&provider.provider_type)),
        SettingsProviderField::BaseUrl => provider.base_url.clone().unwrap_or_default(),
        SettingsProviderField::ApiKey => {
            if provider
                .api_key
                .as_deref()
                .is_some_and(|value| !value.is_empty())
            {
                String::from("••••••")
            } else {
                String::new()
            }
        }
        SettingsProviderField::EnvVarApiKey => provider.env_var_api_key.clone().unwrap_or_default(),
        SettingsProviderField::OnlyListedModels => {
            if provider.only_listed_models {
                String::from("yes")
            } else {
                String::from("no")
            }
        }
    }
}

fn settings_model_field_label(field: SettingsModelField) -> &'static str {
    match field {
        SettingsModelField::Id => "Model ID",
        SettingsModelField::Name => "Display Name",
        SettingsModelField::MaxContext => "Max Context",
    }
}

fn settings_model_field_value(model: &ModelSettings, field: SettingsModelField) -> String {
    match field {
        SettingsModelField::Id => model.id.clone(),
        SettingsModelField::Name => model.name.clone().unwrap_or_default(),
        SettingsModelField::MaxContext => model
            .max_context
            .map(|value| value.to_string())
            .unwrap_or_default(),
    }
}

fn parse_settings_errors(message: &str) -> HashMap<String, String> {
    let mut errors = HashMap::new();
    for line in message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some((field, error)) = line.split_once(": ") {
            errors.insert(field.to_string(), error.to_string());
        }
    }
    errors
}

fn render_command_popup(state: &AppState, area: Rect, input_area: Rect, buf: &mut Buffer) {
    if state.command_popup_dismissed {
        return;
    }
    let Some(prefix) = active_command_prefix(&state.input) else {
        return;
    };
    let matches = slash_command_matches(prefix);
    if matches.is_empty() {
        return;
    }

    let visible_count = matches.len().min(6);
    let popup_height = (visible_count as u16).saturating_add(2);
    let popup_width = area.width.saturating_mul(3) / 5;
    let popup_y = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect::new(
        area.x + 1,
        popup_y,
        popup_width.max(24),
        popup_height.max(3),
    );

    let selected_idx = if state.command_completion_prefix.as_deref() == Some(prefix) {
        state
            .command_completion_index
            .min(matches.len().saturating_sub(1))
    } else {
        0
    };
    let visible_lines = popup_area.height.saturating_sub(2) as usize;
    let scroll_offset = menu_scroll_offset(selected_idx, matches.len(), visible_lines);

    let mut lines = Vec::new();
    for (idx, (command, description)) in matches
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_count)
    {
        let selected = idx == selected_idx;
        let marker = if selected { ">" } else { " " };
        lines.push(Line::styled(
            format!("{marker} /{command:<9} {description}"),
            if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            },
        ));
    }

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title("Command (Tab/Down next, Shift-Tab/Up prev, Enter run)")
                .borders(Borders::ALL),
        )
        .render(popup_area, buf);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let popup = Layout::vertical([
        Constraint::Length((area.height.saturating_sub(height)) / 2),
        Constraint::Length(height),
        Constraint::Min(0),
    ])
    .split(area)[1];

    Layout::horizontal([
        Constraint::Length((area.width.saturating_sub(width)) / 2),
        Constraint::Length(width),
        Constraint::Min(0),
    ])
    .split(popup)[1]
}

fn render_model_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let models = flatten_models_map(&state.models_by_provider);
    let popup_area = centered_rect(area.width.saturating_mul(3) / 4, area.height / 2, area);

    let mut lines = vec![Line::styled(
        "Select model (Enter to choose, Esc to close)",
        Style::default().add_modifier(Modifier::BOLD),
    )];

    if models.is_empty() {
        lines.push(Line::raw("No models available"));
    } else {
        for (idx, (provider, model)) in models.iter().enumerate() {
            let selected = idx == state.model_menu_index;
            let marker = if selected { ">" } else { " " };
            let current = state
                .selected_provider_id
                .as_ref()
                .zip(state.selected_model_id.as_ref())
                .is_some_and(|(p, m)| p == provider && m == &model.id);
            let suffix = if current { " (current)" } else { "" };
            lines.push(Line::styled(
                format!("{marker} {provider} / {}{}", model.name, suffix),
                if selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                },
            ));
        }
    }

    let visible_lines = popup_area.height.saturating_sub(2) as usize;
    let selected_line = if models.is_empty() {
        1
    } else {
        state.model_menu_index.saturating_add(1)
    };
    let scroll_offset = menu_scroll_offset(selected_line, lines.len(), visible_lines);

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(Block::default().title("/model").borders(Borders::ALL))
        .scroll((scroll_offset as u16, 0))
        .render(popup_area, buf);
}

fn menu_scroll_offset(selected_line: usize, total_lines: usize, visible_lines: usize) -> usize {
    if visible_lines == 0 || total_lines <= visible_lines {
        return 0;
    }

    let max_scroll = total_lines - visible_lines;
    selected_line
        .saturating_sub(visible_lines.saturating_sub(1))
        .min(max_scroll)
}

fn model_menu_next_index(current_index: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }

    (current_index + 1) % len
}

fn model_menu_previous_index(current_index: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }

    (current_index + len - 1) % len
}

fn render_settings_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(
        area.width.saturating_mul(9) / 10,
        area.height.saturating_mul(4) / 5,
        area,
    );

    Clear.render(popup_area, buf);
    let outer = Block::default().title("/settings").borders(Borders::ALL);
    let inner = outer.inner(popup_area);
    outer.render(popup_area, buf);

    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .areas(inner);

    let header = if let Some(editor) = state.settings_editor {
        let target = match editor {
            ActiveSettingsEditor::Provider(field) => settings_provider_field_label(field),
            ActiveSettingsEditor::Model(field) => settings_model_field_label(field),
        };
        vec![
            Line::styled(
                "Settings Editor",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::raw(format!("Editing {target}: {}", state.settings_editor_input)),
        ]
    } else {
        vec![
            Line::styled(
                "Settings Editor",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::raw("Tab=next pane, Enter=edit/toggle, a=add, x=delete, s=save, Esc=close"),
        ]
    };
    Paragraph::new(Text::from(header)).render(header_area, buf);

    let [
        providers_area,
        provider_form_area,
        models_area,
        model_form_area,
    ] = Layout::horizontal([
        Constraint::Percentage(24),
        Constraint::Percentage(26),
        Constraint::Percentage(24),
        Constraint::Percentage(26),
    ])
    .areas(body_area);

    let mut provider_lines = vec![Line::styled(
        "Providers",
        pane_style(state.settings_focus == SettingsFocus::ProviderList),
    )];
    if let Some(draft) = &state.settings_draft {
        if draft.providers.is_empty() {
            provider_lines.push(Line::raw("No providers"));
        } else {
            for (idx, provider) in draft.providers.iter().enumerate() {
                let selected = idx == state.settings_provider_index;
                provider_lines.push(Line::styled(
                    format!(
                        "{} {}",
                        if selected { ">" } else { " " },
                        if provider.id.is_empty() {
                            "<new provider>"
                        } else {
                            provider.id.as_str()
                        }
                    ),
                    selection_style(
                        state.settings_focus == SettingsFocus::ProviderList && selected,
                    ),
                ));
            }
        }
    }
    Paragraph::new(Text::from(provider_lines))
        .block(Block::default().borders(Borders::ALL))
        .render(providers_area, buf);

    let mut provider_form_lines = vec![Line::styled(
        "Provider Fields",
        pane_style(state.settings_focus == SettingsFocus::ProviderForm),
    )];
    if let Some(provider) = state
        .settings_draft
        .as_ref()
        .and_then(|draft| draft.providers.get(state.settings_provider_index))
    {
        let fields = state
            .settings_draft
            .as_ref()
            .map(|_| match provider.provider_type {
                ProviderType::OpenAi => vec![
                    SettingsProviderField::Id,
                    SettingsProviderField::Type,
                    SettingsProviderField::BaseUrl,
                    SettingsProviderField::ApiKey,
                    SettingsProviderField::EnvVarApiKey,
                    SettingsProviderField::OnlyListedModels,
                ],
                ProviderType::Google => vec![
                    SettingsProviderField::Id,
                    SettingsProviderField::Type,
                    SettingsProviderField::ApiKey,
                    SettingsProviderField::EnvVarApiKey,
                    SettingsProviderField::OnlyListedModels,
                ],
            })
            .unwrap_or_default();
        for (idx, field) in fields.iter().enumerate() {
            let selected = idx == state.settings_provider_field_index;
            let error_key = match field {
                SettingsProviderField::Id => {
                    format!("providers[{}].id", state.settings_provider_index)
                }
                SettingsProviderField::BaseUrl => {
                    format!("providers[{}].base_url", state.settings_provider_index)
                }
                SettingsProviderField::ApiKey | SettingsProviderField::EnvVarApiKey => {
                    format!("providers[{}].credentials", state.settings_provider_index)
                }
                SettingsProviderField::Type | SettingsProviderField::OnlyListedModels => {
                    String::new()
                }
            };
            let mut line = format!(
                "{} {:<18} {}",
                if selected { ">" } else { " " },
                settings_provider_field_label(*field),
                settings_provider_field_value(provider, *field)
            );
            if let Some(error) = state.settings_errors.get(&error_key)
                && !error_key.is_empty()
            {
                line.push_str(&format!("  ! {error}"));
            }
            provider_form_lines.push(Line::styled(
                line,
                selection_style(state.settings_focus == SettingsFocus::ProviderForm && selected),
            ));
        }
    } else {
        provider_form_lines.push(Line::raw("No provider selected"));
    }
    Paragraph::new(Text::from(provider_form_lines))
        .block(Block::default().borders(Borders::ALL))
        .render(provider_form_area, buf);

    let mut model_lines = vec![Line::styled(
        "Models",
        pane_style(state.settings_focus == SettingsFocus::ModelList),
    )];
    let model_indices = if let Some(provider) = state
        .settings_draft
        .as_ref()
        .and_then(|draft| draft.providers.get(state.settings_provider_index))
    {
        state
            .settings_draft
            .as_ref()
            .map(|draft| {
                draft
                    .models
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, model)| (model.provider_id == provider.id).then_some(idx))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    if model_indices.is_empty() {
        model_lines.push(Line::raw("No models"));
    } else if let Some(draft) = &state.settings_draft {
        for (idx, model_index) in model_indices.iter().enumerate() {
            if let Some(model) = draft.models.get(*model_index) {
                model_lines.push(Line::styled(
                    format!(
                        "{} {}",
                        if idx == state.settings_model_index {
                            ">"
                        } else {
                            " "
                        },
                        if model.id.is_empty() {
                            "<new model>"
                        } else {
                            model.id.as_str()
                        }
                    ),
                    selection_style(
                        state.settings_focus == SettingsFocus::ModelList
                            && idx == state.settings_model_index,
                    ),
                ));
            }
        }
    }
    Paragraph::new(Text::from(model_lines))
        .block(Block::default().borders(Borders::ALL))
        .render(models_area, buf);

    let mut model_form_lines = vec![Line::styled(
        "Model Fields",
        pane_style(state.settings_focus == SettingsFocus::ModelForm),
    )];
    if let Some(model_index) = model_indices.get(state.settings_model_index).copied() {
        if let Some(model) = state
            .settings_draft
            .as_ref()
            .and_then(|draft| draft.models.get(model_index))
        {
            for (idx, field) in SETTINGS_MODEL_FIELDS.iter().enumerate() {
                let selected = idx == state.settings_model_field_index;
                let error_key = match field {
                    SettingsModelField::Id => format!("models[{model_index}].id"),
                    SettingsModelField::Name => String::new(),
                    SettingsModelField::MaxContext => format!("models[{model_index}].max_context"),
                };
                let mut line = format!(
                    "{} {:<18} {}",
                    if selected { ">" } else { " " },
                    settings_model_field_label(*field),
                    settings_model_field_value(model, *field)
                );
                if let Some(error) = state.settings_errors.get(&error_key)
                    && !error_key.is_empty()
                {
                    line.push_str(&format!("  ! {error}"));
                }
                model_form_lines.push(Line::styled(
                    line,
                    selection_style(state.settings_focus == SettingsFocus::ModelForm && selected),
                ));
            }
        }
    } else {
        model_form_lines.push(Line::raw("No model selected"));
    }
    Paragraph::new(Text::from(model_form_lines))
        .block(Block::default().borders(Borders::ALL))
        .render(model_form_area, buf);

    let footer_text = if state.settings_delete_armed {
        String::from("Delete armed: press x again to confirm")
    } else if let Some(editor) = state.settings_editor {
        let field = match editor {
            ActiveSettingsEditor::Provider(field) => settings_provider_field_label(field),
            ActiveSettingsEditor::Model(field) => settings_model_field_label(field),
        };
        format!("Editing {field}: Enter=commit, Esc=cancel")
    } else {
        String::from("Providers and models are shared with the desktop app")
    };
    Paragraph::new(Line::raw(footer_text))
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL))
        .render(footer_area, buf);
}

fn pane_style(active: bool) -> Style {
    if active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn selection_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}

fn render_sessions_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(area.width.saturating_mul(4) / 5, area.height / 2, area);
    let visible_lines = popup_area.height.saturating_sub(2) as usize;
    let total_lines = state.sessions.len() + 2;
    let selected_line = state.sessions_menu_index.saturating_add(1);
    let scroll_offset = menu_scroll_offset(selected_line, total_lines, visible_lines);

    let mut lines = vec![Line::styled(
        "Sessions (Enter=load/new, x=delete, Esc=close)",
        Style::default().add_modifier(Modifier::BOLD),
    )];

    let marker = if state.sessions_menu_index == 0 {
        ">"
    } else {
        " "
    };
    lines.push(Line::styled(
        format!("{marker} Start new chat"),
        if state.sessions_menu_index == 0 {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        },
    ));

    for (idx, session) in state.sessions.iter().enumerate() {
        let selected = state.sessions_menu_index == idx + 1;
        let marker = if selected { ">" } else { " " };
        let current = state
            .current_session_id
            .as_ref()
            .is_some_and(|sid| sid == &session.id);
        let title = session
            .title
            .clone()
            .unwrap_or_else(|| format!("Session {}", &session.id[..8.min(session.id.len())]));
        let suffix = if current { " (current)" } else { "" };
        lines.push(Line::styled(
            format!("{marker} {title}{suffix}"),
            if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            },
        ));
    }

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(Block::default().title("/sessions").borders(Borders::ALL))
        .scroll((scroll_offset as u16, 0))
        .render(popup_area, buf);
}

fn render_tools_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(
        area.width.saturating_mul(4) / 5,
        area.height.saturating_mul(2) / 3,
        area,
    );

    let mut lines = vec![Line::styled(
        "Tools (a=approve, d=deny, e=execute approved, Esc=close)",
        Style::default().add_modifier(Modifier::BOLD),
    )];

    if state.pending_tools.is_empty() {
        lines.push(Line::raw("No pending tool calls"));
    } else {
        for (idx, tool) in state.pending_tools.iter().enumerate() {
            let selected = idx == state.tools_menu_index;
            let marker = if selected { ">" } else { " " };
            let status = match tool.approved {
                Some(true) => "approved",
                Some(false) => "denied",
                None => "pending",
            };
            lines.push(Line::styled(
                format!(
                    "{marker} [{}] {} - {}",
                    status, tool.tool_id, tool.description
                ),
                if selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                },
            ));
            if selected {
                lines.push(Line::styled(
                    format!("    args: {}", tool.args),
                    Style::default().fg(Color::DarkGray),
                ));
                lines.push(Line::styled(
                    format!("    risk: {}", tool.risk_level),
                    Style::default().fg(Color::DarkGray),
                ));
                for reason in &tool.reasons {
                    lines.push(Line::styled(
                        format!("    why: {reason}"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
        }
    }

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(Block::default().title("/tools").borders(Borders::ALL))
        .render(popup_area, buf);
}

fn render_help_menu(area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(area.width.saturating_mul(3) / 5, area.height / 2, area);

    let lines = vec![
        Line::styled(
            "Slash Commands",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw("/help      Open this help menu"),
        Line::raw("/model     Open model selector"),
        Line::raw("/settings  Open settings editor"),
        Line::raw("/sessions  Open sessions menu"),
        Line::raw("/tools     Open tools approval menu"),
        Line::raw("/new       Start a new session"),
        Line::raw("/quit      Exit the TUI"),
        Line::raw(""),
        Line::styled(
            "Chat Navigation",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw("Enter       Send message"),
        Line::raw("Shift+Enter Add newline"),
        Line::raw("Up/Down    Scroll history"),
        Line::raw("PgUp/PgDn  Scroll faster"),
        Line::raw("End        Jump to latest"),
        Line::raw("Home       Jump to top"),
        Line::raw("Drag mouse Select chat text"),
        Line::raw("Ctrl+Shift+C Copy selection"),
        Line::raw(""),
        Line::raw("Esc closes menus."),
    ];

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(Block::default().title("/help").borders(Borders::ALL))
        .render(popup_area, buf);
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
        crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
        layout::Rect,
        style::{Color, Style},
        widgets::Widget,
    };
    use types::{ChatRole, Message, MessageId, MessageStatus};

    use super::{
        ActiveSettingsEditor, App, AppState, ChatCellPosition, ChatSelection,
        OptimisticMessage, PendingTool, RuntimeRequest, RuntimeResponse, SettingsFocus,
        SettingsModelField, SettingsProviderField, UiMode, is_copy_shortcut,
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
        HashMap::from([
            (
                String::from("google"),
                vec![Model {
                    id: String::from("gemini-2.0-flash"),
                    name: String::from("Gemini 2.0 Flash"),
                }],
            ),
            (
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
            ),
        ])
    }

    fn sample_settings() -> SettingsDocument {
        SettingsDocument {
            providers: vec![
                ProviderSettings {
                    id: String::from("openai"),
                    provider_type: ProviderType::OpenAi,
                    base_url: Some(String::from("https://api.openai.com/v1")),
                    api_key: None,
                    env_var_api_key: Some(String::from("OPENAI_API_KEY")),
                    only_listed_models: true,
                },
                ProviderSettings {
                    id: String::from("google"),
                    provider_type: ProviderType::Google,
                    base_url: None,
                    api_key: None,
                    env_var_api_key: Some(String::from("GEMINI_API_KEY")),
                    only_listed_models: true,
                },
            ],
            models: vec![
                ModelSettings {
                    id: String::from("gpt-4o-mini"),
                    provider_id: String::from("openai"),
                    name: Some(String::from("GPT-4o Mini")),
                    max_context: Some(128_000),
                },
                ModelSettings {
                    id: String::from("gemini-2.0-flash"),
                    provider_id: String::from("google"),
                    name: Some(String::from("Gemini 2.0 Flash")),
                    max_context: Some(1_000_000),
                },
            ],
        }
    }

    fn sample_sessions() -> Vec<Session> {
        vec![
            Session {
                id: String::from("sess-1"),
                tip_id: Some(String::from("m2")),
                created_at: 1,
                updated_at: 2,
                title: Some(String::from("Refactor ideas")),
            },
            Session {
                id: String::from("sess-2"),
                tip_id: Some(String::from("m3")),
                created_at: 3,
                updated_at: 4,
                title: Some(String::from("Testing plan")),
            },
        ]
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
            },
            PendingTool {
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
06:          │  google / Gemini 2.0 Flash                         │
07:          │> openai / GPT-4.1 Mini                             │
08:          │  openai / GPT-4o Mini (current)                    │
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
08:      ││  google           ││  Provider Type      ││                   ││  Display Name       ││
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
08:        │> Testing plan (current)                               │
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

        assert_eq!(selection_text(&view, selection), Some(String::from("pha\n\nbet")));
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
        harness.app.state.visible_chat_view =
            Some(visible_chat_view(&["alpha beta", "gamma"], Rect::new(0, 0, 12, 2)));
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
    fn mouse_drag_updates_selection_in_chat_view() {
        let mut harness = test_harness();
        harness.app.state.visible_chat_view =
            Some(visible_chat_view(&["alpha", "beta"], Rect::new(0, 0, 10, 2)));

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
                buffer[(x, y)].set_char(' ').set_style(Style::default().fg(Color::White));
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
        assert_eq!(requests.len(), 2);
        assert!(matches!(requests[0], RuntimeRequest::ListSessions));
        assert!(matches!(requests[1], RuntimeRequest::GetCurrentSessionId));
    }

    #[test]
    fn submit_sends_message_request_and_tracks_optimistic_message() {
        let mut harness = test_harness();
        harness.app.state.config_loaded = true;
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
                message,
                model_id,
                provider_id,
            } => {
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
                message,
                model_id,
                provider_id,
            } => {
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
                message,
                model_id,
                provider_id,
            } => {
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
                message,
                model_id,
                provider_id,
            } => {
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
        harness.app.state.pending_tools = sample_pending_tools();

        harness.app.handle_key_event(key(KeyCode::Char('e')));

        assert_eq!(harness.app.state.mode, UiMode::Chat);
        let requests = harness.drain_requests();
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], RuntimeRequest::ExecuteApprovedTools));
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
    fn tool_call_event_adds_pending_tool_and_status() {
        let mut harness = test_harness();

        harness.app.handle_runtime_event(Event::ToolCallDetected {
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
            RuntimeRequest::SendMessage { .. } => "SendMessage",
            RuntimeRequest::SaveSettings { .. } => "SaveSettings",
            RuntimeRequest::GetChatHistory => "GetChatHistory",
            RuntimeRequest::GetCurrentTip => "GetCurrentTip",
            RuntimeRequest::ClearCurrentSession => "ClearCurrentSession",
            RuntimeRequest::LoadSession { .. } => "LoadSession",
            RuntimeRequest::ListSessions => "ListSessions",
            RuntimeRequest::DeleteSession { .. } => "DeleteSession",
            RuntimeRequest::GetCurrentSessionId => "GetCurrentSessionId",
            RuntimeRequest::ApproveTool { .. } => "ApproveTool",
            RuntimeRequest::DenyTool { .. } => "DenyTool",
            RuntimeRequest::ExecuteApprovedTools => "ExecuteApprovedTools",
        }
    }
}
