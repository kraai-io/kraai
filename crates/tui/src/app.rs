use std::collections::BTreeMap;

use agent_runtime::{Event, RuntimeHandle};
use color_eyre::eyre::Result;
use crossbeam_channel::{Receiver, Sender};
use ratatui::{
    buffer::Buffer,
    crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Flex, Layout, Rect},
    widgets::Widget,
};
use types::{ChatRole, Message, MessageId, MessageStatus};

use crate::components::{ChatHistory, TextInput};

const PROVIDER_ID: &str = "opencode-zen";
const MODEL_ID: &str = "big-pickle";

pub struct App {
    runtime: RuntimeHandle,
    event_rx: Receiver<Event>,
    history_rx: Receiver<BTreeMap<MessageId, Message>>,
    history_tx: Sender<BTreeMap<MessageId, Message>>,
    state: AppState,
}

#[derive(Clone)]
pub struct AppState {
    exit: bool,
    input: String,
    chat_history: BTreeMap<MessageId, Message>,
    scroll: u16,
    auto_scroll: bool,
    config_loaded: bool,
}

impl App {
    pub fn new(
        runtime: RuntimeHandle,
        event_rx: Receiver<Event>,
        history_rx: Receiver<BTreeMap<MessageId, Message>>,
        history_tx: Sender<BTreeMap<MessageId, Message>>,
    ) -> Self {
        let state = AppState {
            exit: false,
            input: String::new(),
            chat_history: BTreeMap::new(),
            scroll: 0,
            auto_scroll: true,
            config_loaded: false,
        };

        Self {
            runtime,
            event_rx,
            history_rx,
            history_tx,
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
                    self.refresh_history();
                }
                Event::Error(msg) => {
                    tracing::error!("Error: {}", msg);
                }
                Event::StreamStart { message_id: _ } => {
                    // Streaming message will appear in history when we refresh
                    // The runtime's get_chat_history includes streaming messages
                }
                Event::StreamChunk { message_id: _, chunk: _ } => {
                    // Streaming content will appear in history when we refresh
                }
                Event::StreamComplete { message_id } => {
                    tracing::debug!("Stream complete: {}", message_id);
                    // Refresh history to get the real message from runtime
                    self.refresh_history();
                }
                Event::StreamError { message_id, error } => {
                    tracing::error!("Stream error for {}: {}", message_id, error);
                }
                Event::HistoryUpdated => {
                    self.refresh_history();
                }
                Event::MessageComplete(_msg) => {}
                Event::ToolCallDetected { .. } => {}
                Event::ToolResultReady { .. } => {}
            }
        }

        while let Ok(history) = self.history_rx.try_recv() {
            self.state.chat_history = history;
            self.state.auto_scroll = true;
        }
    }

    fn refresh_history(&mut self) {
        let runtime = self.runtime.clone();
        let history_tx = self.history_tx.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                match runtime.get_chat_history().await {
                    Ok(history) => {
                        tracing::debug!("Fetched {} messages from history", history.len());
                        let _ = history_tx.send(history);
                    }
                    Err(e) => {
                        tracing::error!("Failed to fetch history: {}", e);
                    }
                }
            });
        });
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

                    let runtime = self.runtime.clone();
                    
                    // Get the current tip from runtime in a separate thread
                    let tip_id: Option<MessageId> = std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new().unwrap();
                        rt.block_on(async {
                            runtime.get_current_tip().await.ok().flatten()
                                .map(MessageId::new)
                        })
                    }).join().unwrap();

                    // Add optimistic user message immediately with parent set to current tip
                    let message_id = MessageId::new(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis()
                            .to_string(),
                    );
                    
                    let optimistic_message = Message {
                        id: message_id,
                        parent_id: tip_id,
                        role: ChatRole::User,
                        content: content.clone(),
                        status: MessageStatus::Complete,
                    };
                    self.state.chat_history.insert(optimistic_message.id.clone(), optimistic_message);

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
            KeyCode::Up => {
                self.state.auto_scroll = false;
                self.state.scroll = self.state.scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                if self.state.auto_scroll {
                    self.state.auto_scroll = false;
                }
                self.state.scroll = self.state.scroll.saturating_add(1);
            }
            _ => {}
        }
    }

    fn exit(&mut self) {
        self.state.exit = true;
    }
}

fn build_ordered_messages(history: &BTreeMap<MessageId, Message>) -> Vec<&Message> {
    if history.is_empty() {
        return Vec::new();
    }

    // Build child map (parent_id -> child_id)
    let mut child_map: std::collections::HashMap<&MessageId, &MessageId> =
        std::collections::HashMap::new();
    for (id, msg) in history.iter() {
        if let Some(parent_id) = &msg.parent_id {
            child_map.insert(parent_id, id);
        }
    }

    // Find tip: message ID that is not a parent of any message (not in child_map values)
    let all_ids: std::collections::HashSet<&MessageId> =
        history.keys().collect();
    let child_ids: std::collections::HashSet<&MessageId> = child_map.values().cloned().collect();
    
    let tip_id = all_ids.difference(&child_ids).next().copied();

    // Walk from tip backwards through parents
    let mut ordered: Vec<&Message> = Vec::new();
    let mut current_id = tip_id;

    while let Some(id) = current_id {
        if let Some(msg) = history.get(id) {
            ordered.push(msg);
            current_id = msg.parent_id.as_ref();
        } else {
            break;
        }
    }

    ordered
}

impl Widget for AppState {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        // Build ordered list from tree by walking from tip to root
        let messages: Vec<&Message> = build_ordered_messages(&self.chat_history);
        
        let input_height = TextInput::new(&self.input).get_height(area.width);
        let layout = Layout::vertical([
            Constraint::Min(area.height.saturating_sub(input_height)),
            Constraint::Length(input_height),
        ])
        .flex(Flex::End);
        let [chat_history_area, input_area] = layout.areas(area);

        ChatHistory::new(&messages, self.scroll, self.auto_scroll).render(chat_history_area, buf);
        TextInput::new(&self.input).render(input_area, buf);
    }
}
