use std::collections::{BTreeMap, HashMap, HashSet};

use agent_runtime::{Event, Model, RuntimeHandle, Session};
use color_eyre::eyre::Result;
use crossbeam_channel::{Receiver, Sender, unbounded};
use ratatui::{
    buffer::Buffer,
    crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use types::{ChatRole, Message, MessageId, MessageStatus};

use crate::components::{ChatHistory, TextInput};

#[derive(Clone, Debug, PartialEq, Eq)]
enum UiMode {
    Chat,
    ModelMenu,
    SessionsMenu,
    ToolsMenu,
    Help,
}

#[derive(Clone, Debug)]
struct PendingTool {
    call_id: String,
    tool_id: String,
    args: String,
    description: String,
    approved: Option<bool>,
}

#[derive(Clone, Debug)]
struct OptimisticMessage {
    local_id: String,
    content: String,
}

enum RuntimeRequest {
    ListModels,
    SendMessage {
        message: String,
        model_id: String,
        provider_id: String,
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
    SendMessage(Result<(), String>),
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
    state: AppState,
}

#[derive(Clone)]
pub struct AppState {
    exit: bool,
    input: String,
    chat_history: BTreeMap<MessageId, Message>,
    optimistic_messages: Vec<OptimisticMessage>,
    optimistic_seq: u64,
    scroll: u16,
    auto_scroll: bool,
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
}

impl App {
    pub fn new(runtime: RuntimeHandle, event_rx: Receiver<Event>) -> Self {
        let (runtime_tx, req_rx): (Sender<RuntimeRequest>, Receiver<RuntimeRequest>) = unbounded();
        let (res_tx, runtime_rx): (Sender<RuntimeResponse>, Receiver<RuntimeResponse>) = unbounded();

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

        let state = AppState {
            exit: false,
            input: String::new(),
            chat_history: BTreeMap::new(),
            optimistic_messages: Vec::new(),
            optimistic_seq: 0,
            scroll: 0,
            auto_scroll: true,
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
        };

        Self {
            event_rx,
            runtime_tx,
            runtime_rx,
            state,
        }
    }

    pub fn run(&mut self, mut terminal: ratatui::DefaultTerminal) -> Result<()> {
        while !self.state.exit {
            self.process_events();

            terminal.draw(|frame| {
                let area = frame.area();
                frame.render_widget(self.state.clone(), area);

                if self.state.mode == UiMode::Chat {
                    let input_height = TextInput::new(&self.state.input).get_height(area.width);
                    let layout = Layout::vertical([
                        Constraint::Min(area.height.saturating_sub(input_height + 1)),
                        Constraint::Length(1),
                        Constraint::Length(input_height),
                    ])
                    .flex(Flex::End);
                    let [_chat_area, _status_area, input_area] = layout.areas(area);

                    let (cursor_x, cursor_y) =
                        TextInput::new(&self.state.input).get_cursor_position(input_area);
                    frame.set_cursor_position((cursor_x, cursor_y));
                }
            })?;

            self.handle_events()?;
        }
        Ok(())
    }

    fn process_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            self.handle_runtime_event(event);
        }

        while let Ok(response) = self.runtime_rx.try_recv() {
            self.handle_runtime_response(response);
        }
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
            }
            Event::StreamChunk { .. } => {
                self.request(RuntimeRequest::GetChatHistory);
            }
            Event::StreamComplete { .. } => {
                self.state.is_streaming = false;
                self.request_sync();
            }
            Event::StreamError { error, .. } => {
                self.state.is_streaming = false;
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
                self.state.pending_tools.retain(|tool| tool.call_id != call_id);
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
            RuntimeResponse::SendMessage(Ok(())) => {}
            RuntimeResponse::SendMessage(Err(err)) => {
                self.state.is_streaming = false;
                self.state.status = format!("Send failed: {err}");
            }
            RuntimeResponse::ChatHistory(Ok(history)) => {
                self.state.chat_history = history;
                self.state.optimistic_messages.clear();
                self.state.auto_scroll = true;
            }
            RuntimeResponse::ChatHistory(Err(err)) => {
                self.state.status = format!("Failed loading history: {err}");
            }
            RuntimeResponse::CurrentTip(Ok(tip)) => {
                self.state.current_tip_id = tip;
            }
            RuntimeResponse::CurrentTip(Err(err)) => {
                self.state.status = format!("Failed loading tip: {err}");
            }
            RuntimeResponse::ClearCurrentSession(Ok(())) => {
                self.state.status = String::from("Started new session");
                self.state.chat_history.clear();
                self.state.optimistic_messages.clear();
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
                self.state.status = String::from("Session loaded");
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

    fn handle_events(&mut self) -> Result<()> {
        if event::poll(std::time::Duration::from_millis(16))? {
            match event::read()? {
                CrosstermEvent::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                    self.handle_key_event(key_event)
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if matches!(key_event.code, KeyCode::Esc) {
            self.state.mode = UiMode::Chat;
            return;
        }

        match self.state.mode {
            UiMode::Chat => self.handle_chat_key_event(key_event),
            UiMode::ModelMenu => self.handle_model_menu_key_event(key_event),
            UiMode::SessionsMenu => self.handle_sessions_menu_key_event(key_event),
            UiMode::ToolsMenu => self.handle_tools_menu_key_event(key_event),
            UiMode::Help => {
                if matches!(key_event.code, KeyCode::Enter | KeyCode::Char('q')) {
                    self.state.mode = UiMode::Chat;
                }
            }
        }
    }

    fn handle_chat_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Enter => self.handle_submit(),
            KeyCode::Char(c) => self.state.input.push(c),
            KeyCode::Backspace => {
                self.state.input.pop();
            }
            KeyCode::Up => {
                self.state.auto_scroll = false;
                self.state.scroll = self.state.scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                self.state.auto_scroll = false;
                self.state.scroll = self.state.scroll.saturating_add(1);
            }
            _ => {}
        }
    }

    fn handle_model_menu_key_event(&mut self, key_event: KeyEvent) {
        let models = self.flatten_models();
        let len = models.len();

        match key_event.code {
            KeyCode::Up => {
                self.state.model_menu_index = self.state.model_menu_index.saturating_sub(1);
            }
            KeyCode::Down => {
                if len > 0 {
                    self.state.model_menu_index = (self.state.model_menu_index + 1).min(len - 1);
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
                self.state.sessions_menu_index = self.state.sessions_menu_index.saturating_sub(1);
            }
            KeyCode::Down => {
                if total > 0 {
                    self.state.sessions_menu_index =
                        (self.state.sessions_menu_index + 1).min(total - 1);
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

    fn handle_submit(&mut self) {
        let raw_input = self.state.input.trim().to_string();
        if raw_input.is_empty() {
            return;
        }
        self.state.input.clear();

        if let Some(command) = raw_input.strip_prefix('/') {
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
                self.state.mode = UiMode::ModelMenu;
                self.request(RuntimeRequest::ListModels);
            }
            "sessions" => {
                self.state.mode = UiMode::SessionsMenu;
                self.request(RuntimeRequest::ListSessions);
                self.request(RuntimeRequest::GetCurrentSessionId);
            }
            "tools" => {
                self.state.mode = UiMode::ToolsMenu;
            }
            "new" => {
                self.request(RuntimeRequest::ClearCurrentSession);
            }
            "help" => {
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

    let tip_id = current_tip_id
        .filter(|id| history.contains_key(&MessageId::new((*id).to_string())))
        .map(|id| MessageId::new(id.to_string()))
        .or_else(|| inferred_tip.map(MessageId::new));

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

impl Widget for AppState {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        let mut rendered_messages: Vec<Message> = build_tip_chain(&self.chat_history, self.current_tip_id.as_deref())
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

        let message_refs: Vec<&Message> = rendered_messages.iter().collect();

        let input_height = TextInput::new(&self.input).get_height(area.width);
        let layout = Layout::vertical([
            Constraint::Min(area.height.saturating_sub(input_height + 1)),
            Constraint::Length(1),
            Constraint::Length(input_height),
        ])
        .flex(Flex::End);
        let [chat_history_area, status_area, input_area] = layout.areas(area);

        ChatHistory::new(&message_refs, self.scroll, self.auto_scroll).render(chat_history_area, buf);

        let selected_model = self
            .selected_provider_id
            .as_ref()
            .zip(self.selected_model_id.as_ref())
            .map(|(p, m)| format!("{p}/{m}"))
            .unwrap_or_else(|| String::from("none"));
        let stream_state = if self.is_streaming { "streaming" } else { "idle" };
        let pending_tools = self.pending_tools.len();
        let status_line = format!(
            "{} | model={} | tools={} | {}",
            self.status, selected_model, pending_tools, stream_state
        );

        Paragraph::new(Line::raw(status_line))
            .style(Style::default().fg(Color::DarkGray))
            .render(status_area, buf);

        TextInput::new(&self.input).render(input_area, buf);

        match self.mode {
            UiMode::ModelMenu => render_model_menu(&self, area, buf),
            UiMode::SessionsMenu => render_sessions_menu(&self, area, buf),
            UiMode::ToolsMenu => render_tools_menu(&self, area, buf),
            UiMode::Help => render_help_menu(area, buf),
            UiMode::Chat => {}
        }
    }
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

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(Block::default().title("/model").borders(Borders::ALL))
        .render(popup_area, buf);
}

fn render_sessions_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(area.width.saturating_mul(4) / 5, area.height / 2, area);

    let mut lines = vec![Line::styled(
        "Sessions (Enter=load/new, x=delete, Esc=close)",
        Style::default().add_modifier(Modifier::BOLD),
    )];

    let marker = if state.sessions_menu_index == 0 { ">" } else { " " };
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
        .render(popup_area, buf);
}

fn render_tools_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(area.width.saturating_mul(4) / 5, area.height.saturating_mul(2) / 3, area);

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
                format!("{marker} [{}] {} - {}", status, tool.tool_id, tool.description),
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
        Line::styled("Slash Commands", Style::default().add_modifier(Modifier::BOLD)),
        Line::raw("/help      Open this help menu"),
        Line::raw("/model     Open model selector"),
        Line::raw("/sessions  Open sessions menu"),
        Line::raw("/tools     Open tools approval menu"),
        Line::raw("/new       Start a new session"),
        Line::raw("/quit      Exit the TUI"),
        Line::raw(""),
        Line::raw("Esc closes menus."),
    ];

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(Block::default().title("/help").borders(Borders::ALL))
        .render(popup_area, buf);
}
