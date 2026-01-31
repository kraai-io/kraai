use color_eyre::eyre::Result;
use provider_core::ProviderManager;
use provider_google::GoogleFactory;
use provider_openai::OpenAIFactory;
use ratatui::{
    DefaultTerminal,
    buffer::Buffer,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Flex, Layout, Rect},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Widget},
};
use types::{ChatMessage, ChatRole};

struct Text {
    content: String,
}

impl Text {
    fn new(content: String) -> Self {
        Self { content }
    }
}

impl Widget for Text {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        Block::bordered()
            .border_type(BorderType::Rounded)
            .render(area, buf);

        let max_width = (area.width - 4) as usize;
        let max_height = (area.height - 2) as usize;
        let lines = self.wrap_text(max_width);

        for (i, line) in lines.iter().enumerate().take(max_height) {
            let y = area.y + 1 + i as u16;
            for (j, ch) in line.chars().enumerate() {
                let x = area.x + 2 + j as u16;
                if x < area.x + area.width - 2 {
                    buf[(x, y)].set_char(ch);
                }
            }
        }
    }
}

impl Text {
    fn get_height(&self, max_width: u16) -> u16 {
        self.wrap_text(max_width as usize - 4).len() as u16 + 2
    }

    fn wrap_text(&self, max_width: usize) -> Vec<String> {
        let mut result = Vec::new();

        for mut line in self.content.lines() {
            if line.len() <= max_width {
                result.push(line.to_string());
            } else {
                // let mut current_line = String::new();
                loop {
                    let (a, b) = line.split_at(max_width);
                    line = b;
                    result.push(a.to_string());

                    if line.len() <= max_width {
                        result.push(line.to_string());
                        break;
                    }
                }

                // for word in line.split_whitespace() {
                //     if current_line.len() + 1 + word.len() <= max_width {
                //         if !current_line.is_empty() {
                //             current_line.push(' ');
                //         }
                //         current_line.push_str(word);
                //     } else if word.len() > max_width {
                //         let (add, new) = word.split_at(max_width - current_line.len());
                //         if !current_line.is_empty() {
                //             current_line.push(' ');
                //         }
                //         current_line.push_str(add);
                //         result.push(current_line.clone());
                //         current_line = new.to_string();
                //     } else {
                //         result.push(current_line.clone());
                //         current_line = word.to_string();
                //     }
                // }
                // if !current_line.is_empty() {
                //     result.push(current_line);
                // }
            }
        }

        result
    }
}

struct TextEdit {
    input: String,
}

impl TextEdit {
    fn new(input: String) -> Self {
        Self { input }
    }
}

impl Widget for TextEdit {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut input = "> ".to_string();
        input.push_str(&self.input);
        Text::new(input).render(area, buf);
    }
}

struct Agent {
    chat_history: Vec<ChatMessage>,
}

impl Agent {
    fn new() -> Self {
        Self {
            chat_history: vec![],
        }
    }
}

pub struct App {
    providers: ProviderManager,
    agent: Agent,
    state: AppState,
}

#[derive(Clone)]
pub struct AppState {
    exit: bool,
    chat_scroll: u16,
    input: String,
    input_height: u16,
    chat_history: Vec<ChatMessage>,
}

impl App {
    pub async fn new() -> Result<Self> {
        let mut providers = ProviderManager::new();
        providers.register_factory::<GoogleFactory>();
        providers.register_factory::<OpenAIFactory>();
        let config_slice = std::fs::read("crates/cli/config/config.toml")?;
        let config = toml::from_slice(&config_slice)?;
        providers.load_config(config).await?;

        let mut agent = Agent::new();

        agent.chat_history.push(ChatMessage {
            role: ChatRole::User,
            content: "what are the best tui options in rust".to_string(),
        });

        agent.chat_history.push(ChatMessage {
            role: ChatRole::User,
            content: "what about ratatui?".to_string(),
        });
        // agent.chat_history.push(
        //     providers
        //         .generate_reply(
        //             "open-webui".to_string().into(),
        //             &"gemma3n:e4b".to_string().into(),
        //             agent.chat_history.clone(),
        //         )
        //         .await?,
        // );
        let input = String::new();
        let input_height = 3; // Default height for empty input

        let state = AppState {
            exit: false,
            input,
            input_height,
            chat_scroll: 0,
            chat_history: agent.chat_history.clone(),
        };

        Ok(Self {
            providers,
            agent,
            state,
        })
    }

    pub async fn run(&mut self, mut terminal: DefaultTerminal) -> Result<()> {
        while !self.state.exit {
            terminal.draw(|frame| {
                let area = frame.area();
                frame.render_widget(self.state.clone(), area);

                let input = format!("> {}", self.state.input).replace(" ", "1");
                let input_height = Text::new(input.clone())
                    .wrap_text(area.width as usize - 4)
                    .len()
                    .max(3)
                    .min(12) as u16;
                let layout = Layout::vertical([
                    Constraint::Min(area.height - input_height),
                    Constraint::Length(input_height),
                ])
                .flex(Flex::End);
                let [_chat_history_area, input_area] = layout.areas(area);

                let cursor_line_idx = input_height.saturating_sub(2);
                let cursor_visual_x_on_line = Text::new(input.clone())
                    .wrap_text(area.width as usize - 4)
                    .last()
                    .unwrap()
                    .len() as u16;
                let cursor_x = input_area.x + cursor_visual_x_on_line + 2;
                let cursor_y = input_area.y + cursor_line_idx;

                frame.set_cursor_position((cursor_x, cursor_y));
            })?;
            self.handle_events()?;
            terminal.show_cursor()?;
        }
        Ok(())
    }

    fn handle_events(&mut self) -> Result<()> {
        match event::read()? {
            // it's important to check that the event is a key press event as
            // crossterm also emits key release and repeat events on Windows.
            Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                self.handle_key_event(key_event)
            }
            _ => {}
        };
        Ok(())
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Char('q') => self.exit(),
            KeyCode::Enter => {
                if !self.state.input.is_empty() {
                    self.state.chat_history.push(ChatMessage {
                        role: ChatRole::User,
                        content: self.state.input.drain(..).collect(),
                    });
                }
            }
            KeyCode::Char(c) => self.state.input.push(c),
            KeyCode::Backspace => {
                self.state.input.pop();
            }
            KeyCode::Down => {
                if self.state.chat_scroll > 0 {
                    self.state.chat_scroll -= 1
                }
            }
            KeyCode::Up => self.state.chat_scroll += 1,
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
        // Update input_height every render step
        let input_height = ((format!("> {}", self.input).len() as u16 / (area.width - 4)) + 3)
            .max(3)
            .min(12);
        let layout = Layout::vertical([
            Constraint::Min(area.height - input_height),
            Constraint::Length(input_height),
        ])
        .flex(Flex::End);
        let [chat_history_area, input_area] = layout.areas(area);
        self.clone().render_chat_history(chat_history_area, buf);
        self.render_input(input_area, buf);
    }
}

impl AppState {
    fn render_chat_history(self, area: Rect, buf: &mut Buffer) {
        let mut total_height = 0;
        let mut widgets = vec![];
        let mut heights = vec![];
        for chat_message in &self.chat_history {
            let text = Text::new(chat_message.content.clone());
            let lines = text.wrap_text((area.width - 2) as usize);
            let height = lines.len() as u16 + 2;
            total_height += height;
            let paragraph = self.chat_message_widget(chat_message);
            widgets.push(paragraph);
            heights.push(height);
        }
        let total_area = Rect::new(0, 0, area.width, total_height);
        let mut total_buf = Buffer::empty(total_area);
        let areas = Layout::vertical(heights).flex(Flex::End).split(total_area);
        for (i, w) in widgets.into_iter().enumerate() {
            let area = areas[i];
            w.render(area, &mut total_buf);
        }

        let skip_amount: usize = (isize::max(
            0,
            total_height as isize - area.height as isize - self.chat_scroll as isize,
        ))
        .try_into()
        .unwrap();
        for (i, cell) in total_buf.content.into_iter().enumerate().skip(skip_amount) {
            let x = i as u16 % area.width;
            let y = i as u16 / area.width;
            buf[(area.x + x, area.y + y)] = cell;
        }
    }

    fn chat_message_widget(&self, chat_message: &ChatMessage) -> impl Widget {
        let text = Text::new(chat_message.content.clone());
        text
    }

    fn render_input(self, area: Rect, buf: &mut Buffer) {
        let text_edit = TextEdit::new(self.input);
        text_edit.render(area, buf);
    }
}
