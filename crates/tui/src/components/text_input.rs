use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    widgets::{Block, BorderType, Widget},
};

pub struct TextInput<'a> {
    input: &'a str,
    cursor: usize,
}

impl<'a> TextInput<'a> {
    pub fn new(input: &'a str, cursor: usize) -> Self {
        Self { input, cursor }
    }

    fn wrap_text(content: &str, max_width: usize) -> Vec<String> {
        if max_width == 0 {
            return vec![String::new()];
        }

        let mut result = Vec::new();
        let mut current = String::new();

        for ch in content.chars() {
            if ch == '\n' {
                result.push(current);
                current = String::new();
                continue;
            }

            if current.chars().count() >= max_width {
                result.push(current);
                current = String::new();
            }
            current.push(ch);
        }

        result.push(current);
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
        let safe_cursor = self
            .cursor
            .min(self.input.len())
            .min(next_char_boundary(self.input, self.cursor));
        let display = format!("> {}", &self.input[..safe_cursor]);
        let max_width = area.width.saturating_sub(4) as usize;
        let lines = Self::wrap_text(&display, max_width);

        let line_count = lines.len();
        let empty = String::new();
        let last_line = lines.last().unwrap_or(&empty);

        let cursor_line_idx = (line_count.saturating_sub(1)) as u16;
        let cursor_x = area.x + 2 + last_line.chars().count() as u16;
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

fn next_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    if s.is_char_boundary(idx) {
        return idx;
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}
