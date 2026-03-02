use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};

pub struct TextInput<'a> {
    input: &'a str,
    cursor: usize,
}

const H_PADDING: u16 = 1;
const V_PADDING: u16 = 1;
const PROMPT: &str = "> ";
const CONTINUATION_PREFIX: &str = "  ";
const INPUT_STYLE: Style = Style::new()
    .fg(Color::Rgb(255, 255, 255))
    .bg(Color::DarkGray);

impl<'a> TextInput<'a> {
    pub fn new(input: &'a str, cursor: usize) -> Self {
        Self { input, cursor }
    }

    fn wrap_text(content: &str, max_width: usize) -> Vec<String> {
        if max_width == 0 {
            return vec![String::new()];
        }

        let mut wrapped = Vec::new();
        let source_lines: Vec<&str> = if content.is_empty() {
            vec![""]
        } else {
            content.split('\n').collect()
        };

        for (idx, source_line) in source_lines.iter().enumerate() {
            let prefix = if idx == 0 {
                PROMPT
            } else {
                CONTINUATION_PREFIX
            };
            let prefix_width = prefix.chars().count();
            let available = max_width.saturating_sub(prefix_width);

            if source_line.is_empty() {
                wrapped.push(prefix.to_string());
                continue;
            }

            if available == 0 {
                wrapped.push(prefix.chars().take(max_width).collect());
                continue;
            }

            let chars: Vec<char> = source_line.chars().collect();
            let mut start = 0usize;
            while start < chars.len() {
                let end = (start + available).min(chars.len());
                let line_prefix = if start == 0 {
                    prefix
                } else {
                    CONTINUATION_PREFIX
                };
                let chunk: String = chars[start..end].iter().collect();
                wrapped.push(format!("{line_prefix}{chunk}"));
                start = end;
            }
        }

        if wrapped.is_empty() {
            wrapped.push(PROMPT.to_string());
        }

        wrapped
    }

    pub fn get_height(&self, max_width: u16) -> u16 {
        let content_width = max_width.saturating_sub(H_PADDING * 2) as usize;
        Self::wrap_text(self.input, content_width).len().max(1) as u16 + (V_PADDING * 2)
    }

    pub fn get_cursor_position(&self, area: Rect) -> (u16, u16) {
        let safe_cursor = self
            .cursor
            .min(self.input.len())
            .min(next_char_boundary(self.input, self.cursor));
        let max_width = area.width.saturating_sub(H_PADDING * 2) as usize;
        let lines = Self::wrap_text(&self.input[..safe_cursor], max_width);

        let line_count = lines.len();
        let empty = String::new();
        let last_line = lines.last().unwrap_or(&empty);

        let cursor_line_idx = (line_count.saturating_sub(1)) as u16;
        let cursor_x = area.x + H_PADDING + last_line.chars().count() as u16;
        let cursor_y = area.y + V_PADDING + cursor_line_idx;

        (cursor_x, cursor_y)
    }
}

impl<'a> Widget for TextInput<'a> {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                buf[(x, y)].set_char(' ').set_style(INPUT_STYLE);
            }
        }

        let max_width = area.width.saturating_sub(H_PADDING * 2) as usize;
        let lines = Self::wrap_text(self.input, max_width);

        for (i, line) in lines.iter().enumerate() {
            let y = area.y + V_PADDING + i as u16;
            for (j, ch) in line.chars().enumerate() {
                let x = area.x + H_PADDING + j as u16;
                if x < area.x + area.width.saturating_sub(H_PADDING) && y < area.y + area.height {
                    let cell = &mut buf[(x, y)];
                    cell.set_char(ch);
                    cell.set_style(INPUT_STYLE);
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
