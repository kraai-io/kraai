use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::Style,
    widgets::{Block, BorderType, Widget},
};
use types::{ChatMessage, ChatRole};

pub struct ChatHistory<'a> {
    messages: &'a [ChatMessage],
    scroll: u16,
}

impl<'a> ChatHistory<'a> {
    pub fn new(messages: &'a [ChatMessage], scroll: u16) -> Self {
        Self { messages, scroll }
    }

    fn wrap_text(&self, content: &str, max_width: usize) -> Vec<String> {
        let mut result = Vec::new();

        for mut line in content.lines() {
            if line.len() <= max_width {
                result.push(line.to_string());
            } else {
                loop {
                    let (a, b) = line.split_at(max_width);
                    line = b;
                    result.push(a.to_string());

                    if line.len() <= max_width {
                        result.push(line.to_string());
                        break;
                    }
                }
            }
        }

        result
    }

    fn get_message_height(&self, content: &str, max_width: u16) -> u16 {
        self.wrap_text(content, max_width as usize - 4).len() as u16 + 2
    }
}

impl<'a> Widget for ChatHistory<'a> {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        Block::bordered()
            .border_type(BorderType::Rounded)
            .render(area, buf);

        let inner_area = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
        let max_width = inner_area.width;

        let heights: Vec<u16> = self
            .messages
            .iter()
            .map(|msg| self.get_message_height(&msg.content, max_width))
            .collect();

        let total_height: u16 = heights.iter().sum();

        let total_area = Rect::new(0, 0, inner_area.width, total_height);
        let mut total_buf = Buffer::empty(total_area);

        let areas = Layout::vertical(heights.iter().map(|&h| Constraint::Length(h)))
            .flex(Flex::Start)
            .split(total_area);

        for (i, msg) in self.messages.iter().enumerate() {
            let msg_area = areas[i];
            let style = match msg.role {
                ChatRole::User => Style::default().fg(ratatui::style::Color::Cyan),
                ChatRole::Assistant => Style::default().fg(ratatui::style::Color::Green),
                _ => Style::default(),
            };

            let wrapped = self.wrap_text(&msg.content, max_width as usize);
            for (line_idx, line) in wrapped.iter().enumerate() {
                let y = msg_area.y + line_idx as u16;
                for (char_idx, ch) in line.chars().enumerate() {
                    let x = msg_area.x + char_idx as u16;
                    if x < msg_area.x + msg_area.width && y < msg_area.y + msg_area.height {
                        let cell = &mut total_buf[(x, y)];
                        cell.set_char(ch);
                        cell.set_style(style);
                    }
                }
            }
        }

        let skip_amount: usize = (isize::max(
            0,
            total_height as isize - inner_area.height as isize - self.scroll as isize,
        ) as usize)
            .min(total_height as usize);

        for (i, cell) in total_buf.content.into_iter().enumerate().skip(skip_amount) {
            let x = i as u16 % inner_area.width;
            let y = i as u16 / inner_area.width;
            if y < inner_area.height {
                buf[(inner_area.x + x, inner_area.y + y)] = cell;
            }
        }
    }
}
