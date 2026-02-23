use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    widgets::{Block, BorderType, Widget},
};

pub struct TextInput<'a> {
    input: &'a str,
}

impl<'a> TextInput<'a> {
    pub fn new(input: &'a str) -> Self {
        Self { input }
    }

    fn wrap_text(content: &str, max_width: usize) -> Vec<String> {
        let mut result = Vec::new();

        let mut line = content.to_string();
        if line.len() <= max_width {
            result.push(line);
        } else {
            loop {
                let (a, b) = line.split_at(max_width);
                result.push(a.to_string());
                line = b.to_string();

                if line.len() <= max_width {
                    result.push(line);
                    break;
                }
            }
        }

        result
    }

    pub fn get_height(&self, max_width: u16) -> u16 {
        let display = format!("> {}", self.input);
        Self::wrap_text(&display, max_width as usize - 4)
            .len()
            .max(1) as u16
            + 2
    }

    pub fn get_cursor_position(&self, area: Rect) -> (u16, u16) {
        let display = format!("> {}", self.input);
        let max_width = area.width.saturating_sub(4) as usize;
        let lines = Self::wrap_text(&display, max_width);

        let line_count = lines.len();
        let empty = String::new();
        let last_line = lines.last().unwrap_or(&empty);

        let cursor_line_idx = (line_count.saturating_sub(1)) as u16;
        let cursor_x = area.x + 2 + last_line.len() as u16;
        let cursor_y = area.y + 1 + cursor_line_idx;

        (cursor_x, cursor_y)
    }
}

impl<'a> Widget for TextInput<'a> {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        Block::bordered()
            .border_type(BorderType::Rounded)
            .render(area, buf);

        let display = format!("> {}", self.input);
        let max_width = (area.width.saturating_sub(4)) as usize;
        let lines = Self::wrap_text(&display, max_width);

        let style = Style::default().fg(ratatui::style::Color::Yellow);

        for (i, line) in lines.iter().enumerate() {
            let y = area.y + 1 + i as u16;
            for (j, ch) in line.chars().enumerate() {
                let x = area.x + 2 + j as u16;
                if x < area.x + area.width - 2 && y < area.y + area.height - 1 {
                    let cell = &mut buf[(x, y)];
                    cell.set_char(ch);
                    cell.set_style(style);
                }
            }
        }
    }
}
