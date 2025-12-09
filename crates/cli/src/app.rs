use color_eyre::eyre::Result;
use provider_core::ProviderManager;
use provider_google::GoogleFactory;
use provider_openai::OpenAIFactory;
use ratatui::{
    DefaultTerminal,
    buffer::Buffer,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Flex, Layout, Rect},
    widgets::{Block, BorderType, Borders, Paragraph, Widget},
};
use types::{ChatMessage, ChatRole};

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
    chat_history: Vec<ChatMessage>,
}

impl App {
    pub async fn new() -> Result<Self> {
        let mut providers = ProviderManager::new();
        providers.register_factory::<GoogleFactory>();
        providers.register_factory::<OpenAIFactory>();
        let config_slice = std::fs::read("./crates/cli/config/config.toml")?;
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
        let input = "hi".to_string();

        let state = AppState {
            exit: false,
            input,

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
            terminal.draw(|frame| frame.render_widget(self.state.clone(), frame.area()))?;
            self.handle_events()?;
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
        let layout = Layout::vertical([Constraint::Min(area.height - 12), Constraint::Max(12)])
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
            let height =
                textwrap::wrap(&chat_message.content, (area.width - 2) as usize).len() as u16 + 2;
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

        let skip_amount = (isize::max(
            0,
            total_height as isize - area.height as isize - self.chat_scroll as isize,
        ) as u16
            * area.width) as usize;
        let visible_content = total_buf
            .content
            .into_iter()
            .skip(skip_amount)
            .take(area.area() as usize);
        for (i, cell) in visible_content.enumerate() {
            let x = i as u16 % area.width;
            let y = i as u16 / area.width;
            buf[(area.x + x, area.y + y)] = cell;
        }
    }

    fn chat_message_widget(&self, chat_message: &ChatMessage) -> impl Widget {
        Paragraph::new(chat_message.content.clone())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(serde_json::to_string(&chat_message.role).unwrap()),
            )
            .wrap(ratatui::widgets::Wrap { trim: false })
    }

    fn render_input(self, area: Rect, buf: &mut Buffer) {
        // let height = textwrap::wrap(&self.input, (area.width - 2) as usize).len() as u16;
        // let input_height = 10.min(height);
        let paragraph = Paragraph::new(self.input)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded),
            )
            .wrap(ratatui::widgets::Wrap { trim: false });
        paragraph.render(area, buf);
    }
}
