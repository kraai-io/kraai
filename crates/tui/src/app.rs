use std::collections::HashMap;

use agent_runtime::{Event, RuntimeHandle};
use color_eyre::eyre::Result;
use crossbeam_channel::Receiver;
use ratatui::{
    buffer::Buffer,
    crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Flex, Layout, Rect},
    widgets::Widget,
};
use types::{ChatMessage, ChatRole};

use crate::components::{ChatHistory, TextInput};

const PROVIDER_ID: &str = "opencode-zen";
const MODEL_ID: &str = "big-pickle";

pub struct App {
    runtime: RuntimeHandle,
    event_rx: Receiver<Event>,
    state: AppState,
}

#[derive(Clone)]
pub struct AppState {
    exit: bool,
    input: String,
    chat_history: Vec<ChatMessage>,
    scroll: u16,
    streaming_content: HashMap<String, String>,
    current_streaming_id: Option<String>,
    config_loaded: bool,
}

impl App {
    pub fn new(runtime: RuntimeHandle, event_rx: Receiver<Event>) -> Self {
        let state = AppState {
            exit: false,
            input: String::new(),
            chat_history: Vec::new(),
            scroll: 0,
            streaming_content: HashMap::new(),
            current_streaming_id: None,
            config_loaded: false,
        };

        Self {
            runtime,
            event_rx,
            state,
        }
    }

    pub fn run(&mut self, mut terminal: ratatui::DefaultTerminal) -> Result<()> {
        while !self.state.exit {
            self.process_events();

            terminal.draw(|frame| {
                let area = frame.area();
                frame.render_widget(self.state.clone(), area);

                let input_height = TextInput::new(&self.state.input).get_height(area.width);
                let layout = Layout::vertical([
                    Constraint::Min(area.height.saturating_sub(input_height)),
                    Constraint::Length(input_height),
                ])
                .flex(Flex::End);
                let [_chat_history_area, input_area] = layout.areas(area);

                let (cursor_x, cursor_y) =
                    TextInput::new(&self.state.input).get_cursor_position(input_area);
                frame.set_cursor_position((cursor_x, cursor_y));
            })?;

            self.handle_events()?;
        }
        Ok(())
    }

    fn process_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                Event::ConfigLoaded => {
                    self.state.config_loaded = true;
                }
                Event::Error(msg) => {
                    eprintln!("Error: {}", msg);
                }
                Event::StreamStart { message_id } => {
                    self.state
                        .streaming_content
                        .insert(message_id.clone(), String::new());
                    self.state.current_streaming_id = Some(message_id);
                    self.state.chat_history.push(ChatMessage {
                        role: ChatRole::Assistant,
                        content: String::new(),
                    });
                }
                Event::StreamChunk { message_id, chunk } => {
                    if let Some(content) = self.state.streaming_content.get_mut(&message_id) {
                        content.push_str(&chunk);
                        if let Some(last) = self.state.chat_history.last_mut()
                            && last.role == ChatRole::Assistant
                        {
                            last.content = content.clone();
                        }
                    }
                }
                Event::StreamComplete { message_id } => {
                    if let Some(content) = self.state.streaming_content.remove(&message_id)
                        && let Some(last) = self.state.chat_history.last_mut()
                        && last.role == ChatRole::Assistant
                        && last.content.is_empty()
                    {
                        last.content = content;
                    }
                    self.state.current_streaming_id = None;
                }
                Event::StreamError { message_id, error } => {
                    eprintln!("Stream error for {}: {}", message_id, error);
                    self.state.streaming_content.remove(&message_id);
                    self.state.current_streaming_id = None;
                }
                Event::HistoryUpdated => {}
                Event::MessageComplete(_msg) => {}
                Event::ToolCallDetected { .. } => {}
                Event::ToolResultReady { .. } => {}
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
        match key_event.code {
            KeyCode::Char('q') => self.exit(),
            KeyCode::Enter => {
                if !self.state.input.is_empty() && self.state.config_loaded {
                    let content: String = self.state.input.drain(..).collect();
                    self.state.chat_history.push(ChatMessage {
                        role: ChatRole::User,
                        content: content.clone(),
                    });

                    let runtime = self.runtime.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new().unwrap();
                        rt.block_on(async {
                            let _ = runtime
                                .send_message(
                                    content,
                                    MODEL_ID.to_string(),
                                    PROVIDER_ID.to_string(),
                                )
                                .await;
                        });
                    });
                }
            }
            KeyCode::Char(c) => self.state.input.push(c),
            KeyCode::Backspace => {
                self.state.input.pop();
            }
            KeyCode::Down => {
                if self.state.scroll > 0 {
                    self.state.scroll -= 1
                }
            }
            KeyCode::Up => self.state.scroll += 1,
            _ => {}
        }
    }

    fn exit(&mut self) {
        self.state.exit = true;
    }
}

impl Widget for AppState {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        let input_height = TextInput::new(&self.input).get_height(area.width);
        let layout = Layout::vertical([
            Constraint::Min(area.height.saturating_sub(input_height)),
            Constraint::Length(input_height),
        ])
        .flex(Flex::End);
        let [chat_history_area, input_area] = layout.areas(area);

        ChatHistory::new(&self.chat_history, self.scroll).render(chat_history_area, buf);
        TextInput::new(&self.input).render(input_area, buf);
    }
}
