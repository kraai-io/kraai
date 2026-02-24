use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    widgets::{Block, BorderType, Widget},
};
use types::{ChatMessage, ChatRole};

pub struct ChatHistory<'a> {
    messages: &'a [ChatMessage],
    scroll: u16,
    auto_scroll: bool,
}

impl<'a> ChatHistory<'a> {
    pub fn new(messages: &'a [ChatMessage], scroll: u16, auto_scroll: bool) -> Self {
        Self {
            messages,
            scroll,
            auto_scroll,
        }
    }

    fn wrap_text(content: &str, max_width: usize) -> Vec<String> {
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

    fn get_message_height(&self, content: &str, max_width: u16, role: &ChatRole) -> u16 {
        let wrapped = Self::wrap_text(content, max_width as usize);
        let lines = wrapped.len().max(1) as u16;
        match role {
            ChatRole::User | ChatRole::Tool => lines + 2,
            ChatRole::Assistant | ChatRole::System => lines,
        }
    }
}

impl<'a> Widget for ChatHistory<'a> {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        if area.width < 4 || area.height < 2 || self.messages.is_empty() {
            return;
        }

        let total_height: u16 = self
            .messages
            .iter()
            .map(|m| self.get_message_height(&m.content, area.width, &m.role))
            .sum();

        let max_scroll = total_height.saturating_sub(area.height);

        let scroll = if self.auto_scroll {
            max_scroll
        } else {
            self.scroll.min(max_scroll)
        };

        let mut accumulated: u16 = 0;
        let mut visible_start = 0;
        let mut lines_to_skip: u16 = 0;

        for (i, msg) in self.messages.iter().enumerate() {
            let height = self.get_message_height(&msg.content, area.width, &msg.role);

            if accumulated + height > scroll {
                visible_start = i;
                lines_to_skip = scroll.saturating_sub(accumulated);
                break;
            }
            accumulated = accumulated.saturating_add(height);
        }

        let mut current_y = area.y;
        let mut remaining_height = area.height;

        for msg in &self.messages[visible_start..] {
            if remaining_height == 0 {
                break;
            }
            let used = self.render_message_partial(
                msg,
                area,
                buf,
                current_y,
                lines_to_skip,
                remaining_height,
            );
            current_y = current_y.saturating_add(used);
            remaining_height = remaining_height.saturating_sub(used);
            lines_to_skip = 0;
        }
    }
}

impl<'a> ChatHistory<'a> {
    fn render_message_partial(
        &self,
        msg: &ChatMessage,
        area: Rect,
        buf: &mut Buffer,
        start_y: u16,
        lines_to_skip: u16,
        max_height: u16,
    ) -> u16 {
        match msg.role {
            ChatRole::User => {
                self.render_user_message_partial(msg, area, buf, start_y, lines_to_skip, max_height)
            }
            ChatRole::Assistant => self.render_assistant_message_partial(
                msg,
                area,
                buf,
                start_y,
                lines_to_skip,
                max_height,
            ),
            ChatRole::Tool => {
                self.render_tool_message_partial(msg, area, buf, start_y, lines_to_skip, max_height)
            }
            ChatRole::System => self.render_system_message_partial(
                msg,
                area,
                buf,
                start_y,
                lines_to_skip,
                max_height,
            ),
        }
    }

    fn render_user_message_partial(
        &self,
        msg: &ChatMessage,
        area: Rect,
        buf: &mut Buffer,
        start_y: u16,
        lines_to_skip: u16,
        max_height: u16,
    ) -> u16 {
        let color = ratatui::style::Color::Cyan;
        let style = Style::default().fg(color);

        let inner_width = area.width.saturating_sub(2) as usize;
        let wrapped = Self::wrap_text(&msg.content, inner_width);
        let total_lines = wrapped.len() + 2;

        let skip_usize = lines_to_skip as usize;
        let available = max_height as usize;

        let render_from = skip_usize.min(total_lines);
        let render_count = available.min(total_lines.saturating_sub(render_from));

        if render_count == 0 {
            return 0;
        }

        for line_offset in 0..render_count {
            let line_idx = render_from + line_offset;
            let y = start_y + line_offset as u16;

            match line_idx {
                0 => {
                    let border_area = Rect::new(area.x, y, area.width, 1);
                    Block::bordered()
                        .border_type(BorderType::Thick)
                        .border_style(style)
                        .render(border_area, buf);
                }
                n if n == total_lines - 1 => {
                    let border_area = Rect::new(area.x, y, area.width, 1);
                    Block::bordered()
                        .border_type(BorderType::Thick)
                        .border_style(style)
                        .render(border_area, buf);
                }
                content_idx => {
                    buf[(area.x, y)].set_char('│').set_style(style);
                    buf[(area.x + area.width - 1, y)]
                        .set_char('│')
                        .set_style(style);

                    let text_line_idx = content_idx - 1;
                    if text_line_idx < wrapped.len() {
                        let line = &wrapped[text_line_idx];
                        for (char_idx, ch) in line.chars().enumerate() {
                            let x = area.x + 1 + char_idx as u16;
                            if x < area.x + area.width - 1 {
                                buf[(x, y)].set_char(ch).set_style(style);
                            }
                        }
                    }
                }
            }
        }

        render_count as u16
    }

    fn render_assistant_message_partial(
        &self,
        msg: &ChatMessage,
        area: Rect,
        buf: &mut Buffer,
        start_y: u16,
        lines_to_skip: u16,
        max_height: u16,
    ) -> u16 {
        let color = ratatui::style::Color::Green;
        let style = Style::default().fg(color);

        let wrapped = Self::wrap_text(&msg.content, area.width as usize);
        let total_lines = wrapped.len().max(1);

        let skip_usize = lines_to_skip as usize;
        let available = max_height as usize;

        let render_from = skip_usize.min(total_lines);
        let render_count = available.min(total_lines.saturating_sub(render_from));

        if render_count == 0 {
            return 0;
        }

        for line_offset in 0..render_count {
            let line_idx = render_from + line_offset;
            let y = start_y + line_offset as u16;

            if line_idx < wrapped.len() {
                let line = &wrapped[line_idx];
                for (char_idx, ch) in line.chars().enumerate() {
                    let x = area.x + char_idx as u16;
                    if x < area.x + area.width {
                        buf[(x, y)].set_char(ch).set_style(style);
                    }
                }
            }
        }

        render_count as u16
    }

    fn render_tool_message_partial(
        &self,
        msg: &ChatMessage,
        area: Rect,
        buf: &mut Buffer,
        start_y: u16,
        lines_to_skip: u16,
        max_height: u16,
    ) -> u16 {
        let color = ratatui::style::Color::Yellow;
        let style = Style::default().fg(color);

        let inner_width = area.width.saturating_sub(2) as usize;
        let wrapped = Self::wrap_text(&msg.content, inner_width);
        let total_lines = wrapped.len() + 2;

        let skip_usize = lines_to_skip as usize;
        let available = max_height as usize;

        let render_from = skip_usize.min(total_lines);
        let render_count = available.min(total_lines.saturating_sub(render_from));

        if render_count == 0 {
            return 0;
        }

        for line_offset in 0..render_count {
            let line_idx = render_from + line_offset;
            let y = start_y + line_offset as u16;

            match line_idx {
                0 => {
                    let border_area = Rect::new(area.x, y, area.width, 1);
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(style)
                        .render(border_area, buf);
                }
                n if n == total_lines - 1 => {
                    let border_area = Rect::new(area.x, y, area.width, 1);
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(style)
                        .render(border_area, buf);
                }
                content_idx => {
                    buf[(area.x, y)].set_char('│').set_style(style);
                    buf[(area.x + area.width - 1, y)]
                        .set_char('│')
                        .set_style(style);

                    let text_line_idx = content_idx - 1;
                    if text_line_idx < wrapped.len() {
                        let line = &wrapped[text_line_idx];
                        for (char_idx, ch) in line.chars().enumerate() {
                            let x = area.x + 1 + char_idx as u16;
                            if x < area.x + area.width - 1 {
                                buf[(x, y)].set_char(ch).set_style(style);
                            }
                        }
                    }
                }
            }
        }

        render_count as u16
    }

    fn render_system_message_partial(
        &self,
        msg: &ChatMessage,
        area: Rect,
        buf: &mut Buffer,
        start_y: u16,
        lines_to_skip: u16,
        max_height: u16,
    ) -> u16 {
        let color = ratatui::style::Color::Gray;
        let style = Style::default().fg(color);

        let wrapped = Self::wrap_text(&msg.content, area.width as usize);
        let total_lines = wrapped.len().max(1);

        let skip_usize = lines_to_skip as usize;
        let available = max_height as usize;

        let render_from = skip_usize.min(total_lines);
        let render_count = available.min(total_lines.saturating_sub(render_from));

        if render_count == 0 {
            return 0;
        }

        for line_offset in 0..render_count {
            let line_idx = render_from + line_offset;
            let y = start_y + line_offset as u16;

            if line_idx < wrapped.len() {
                let line = &wrapped[line_idx];
                for (char_idx, ch) in line.chars().enumerate() {
                    let x = area.x + char_idx as u16;
                    if x < area.x + area.width {
                        buf[(x, y)].set_char(ch).set_style(style);
                    }
                }
            }
        }

        render_count as u16
    }
}
