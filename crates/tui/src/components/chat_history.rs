use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};
use types::{ChatRole, Message};

pub struct ChatHistory<'a> {
    messages: &'a [&'a Message],
    scroll: u16,
    auto_scroll: bool,
}

struct RenderedLine {
    text: String,
    style: Style,
}

impl<'a> ChatHistory<'a> {
    pub fn new(messages: &'a [&'a Message], scroll: u16, auto_scroll: bool) -> Self {
        Self {
            messages,
            scroll,
            auto_scroll,
        }
    }

    fn wrap_with_prefix(
        text: &str,
        width: usize,
        first_prefix: &str,
        cont_prefix: &str,
    ) -> Vec<String> {
        if width == 0 {
            return Vec::new();
        }

        let mut wrapped = Vec::new();
        let mut first_visual_line = true;

        let source_lines: Vec<&str> = if text.is_empty() {
            vec![""]
        } else {
            text.lines().collect()
        };

        for source_line in source_lines {
            let mut chars: Vec<char> = source_line.chars().collect();
            if chars.is_empty() {
                let prefix = if first_visual_line {
                    first_prefix
                } else {
                    cont_prefix
                };
                wrapped.push(Self::fit_to_width(prefix, width));
                first_visual_line = false;
                continue;
            }

            loop {
                let prefix = if first_visual_line {
                    first_prefix
                } else {
                    cont_prefix
                };
                let prefix_width = prefix.chars().count();
                let available = width.saturating_sub(prefix_width);

                if available == 0 {
                    wrapped.push(Self::fit_to_width(prefix, width));
                    first_visual_line = false;
                    continue;
                }

                let take_count = available.min(chars.len());
                let chunk: String = chars.drain(0..take_count).collect();
                wrapped.push(format!("{prefix}{chunk}"));
                first_visual_line = false;

                if chars.is_empty() {
                    break;
                }
            }
        }

        if wrapped.is_empty() {
            wrapped.push(Self::fit_to_width(first_prefix, width));
        }

        wrapped
    }

    fn fit_to_width(content: &str, width: usize) -> String {
        content.chars().take(width).collect()
    }

    fn compact_whitespace(content: &str) -> String {
        content.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn extract_value(line: &str) -> String {
        let trimmed = line.trim();
        if let Some((_, value)) = trimmed.split_once(':') {
            return Self::compact_whitespace(value);
        }
        Self::compact_whitespace(trimmed)
    }

    fn compact_tool_summary(content: &str) -> String {
        let lines: Vec<&str> = content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();

        if lines.is_empty() {
            return String::from("call unknown");
        }

        let mut call: Option<String> = None;
        let mut args: Option<String> = None;
        let mut error: Option<String> = None;

        for line in &lines {
            let lower = line.to_ascii_lowercase();

            if call.is_none() && (lower.contains("tool") || lower.contains("call")) {
                call = Some(Self::extract_value(line));
            }
            if args.is_none()
                && (lower.contains("args")
                    || lower.contains("argument")
                    || line.trim_start().starts_with('{')
                    || line.trim_start().starts_with('['))
            {
                args = Some(Self::extract_value(line));
            }
            if error.is_none()
                && (lower.contains("error") || lower.contains("failed") || lower.contains("denied"))
            {
                error = Some(Self::extract_value(line));
            }
        }

        if call.is_none() {
            call = Some(Self::compact_whitespace(lines[0]));
        }

        let mut parts = Vec::new();
        if let Some(call) = call {
            parts.push(format!("call {call}"));
        }
        if let Some(args) = args {
            parts.push(format!("args {args}"));
        }
        if let Some(error) = error {
            parts.push(format!("error {error}"));
        }

        parts.join(" | ")
    }

    fn build_rendered_lines(&self, width: u16) -> Vec<RenderedLine> {
        let width = width as usize;
        let mut rendered = Vec::new();

        for msg in self.messages {
            if msg.role == ChatRole::System {
                continue;
            }

            if !rendered.is_empty() {
                rendered.push(RenderedLine {
                    text: String::new(),
                    style: Style::default(),
                });
            }

            match msg.role {
                ChatRole::User => {
                    let lines = Self::wrap_with_prefix(&msg.content, width, "", "");
                    for line in lines {
                        rendered.push(RenderedLine {
                            text: line,
                            style: Style::default().fg(Color::White).bg(Color::DarkGray),
                        });
                    }
                }
                ChatRole::Assistant => {
                    let lines = Self::wrap_with_prefix(&msg.content, width, "", "");
                    for line in lines {
                        rendered.push(RenderedLine {
                            text: line,
                            style: Style::default().fg(Color::Green),
                        });
                    }
                }
                ChatRole::Tool => {
                    let compact = Self::compact_tool_summary(&msg.content);
                    let lines = Self::wrap_with_prefix(&compact, width, "tool: ", "      ");
                    for line in lines {
                        rendered.push(RenderedLine {
                            text: line,
                            style: Style::default().fg(Color::Yellow),
                        });
                    }
                }
                ChatRole::System => {}
            }
        }

        rendered
    }

    pub fn max_scroll(messages: &'a [&'a Message], width: u16, height: u16) -> u16 {
        if width == 0 || height == 0 {
            return 0;
        }
        let lines_len = Self::new(messages, 0, true)
            .build_rendered_lines(width)
            .len() as u16;
        lines_len.saturating_sub(height)
    }
}

impl<'a> Widget for ChatHistory<'a> {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let lines = self.build_rendered_lines(area.width);
        if lines.is_empty() {
            return;
        }

        let total_height = lines.len() as u16;
        let max_scroll = total_height.saturating_sub(area.height);
        let scroll = if self.auto_scroll {
            max_scroll
        } else {
            self.scroll.min(max_scroll)
        };

        let start_idx = scroll as usize;
        let end_idx = start_idx
            .saturating_add(area.height as usize)
            .min(lines.len());

        for (visual_idx, line) in lines[start_idx..end_idx].iter().enumerate() {
            let y = area.y + visual_idx as u16;

            for x_offset in 0..area.width {
                buf[(area.x + x_offset, y)]
                    .set_char(' ')
                    .set_style(Style::default());
            }

            for (char_idx, ch) in line.text.chars().enumerate() {
                let x = area.x + char_idx as u16;
                if x >= area.x + area.width {
                    break;
                }
                buf[(x, y)].set_char(ch).set_style(line.style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{MessageId, MessageStatus};

    fn message(id: &str, role: ChatRole, content: &str) -> Message {
        Message {
            id: MessageId::new(id),
            parent_id: None,
            role,
            content: content.to_string(),
            status: MessageStatus::Complete,
        }
    }

    #[test]
    fn wraps_unicode_without_panicking() {
        let wrapped = ChatHistory::wrap_with_prefix("hello 👋 world", 8, "", "");
        assert!(!wrapped.is_empty());
        assert!(wrapped.iter().all(|line| line.chars().count() <= 8));
    }

    #[test]
    fn filters_system_messages_from_rendered_lines() {
        let system = message("1", ChatRole::System, "internal");
        let assistant = message("2", ChatRole::Assistant, "visible");
        let refs = [&system, &assistant];

        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(40);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "visible");
    }

    #[test]
    fn renders_tool_messages_in_compact_form() {
        let tool = message(
            "1",
            ChatRole::Tool,
            "tool_id: read_file\nargs: {\"path\":\"foo.txt\"}\nerror: denied",
        );
        let refs = [&tool];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);

        assert_eq!(lines.len(), 1);
        assert!(lines[0].text.contains("tool:"));
        assert!(lines[0].text.contains("call read_file"));
        assert!(lines[0].text.contains("args {\"path\":\"foo.txt\"}"));
        assert!(lines[0].text.contains("error denied"));
    }

    #[test]
    fn renders_user_messages_without_prefix() {
        let user = message("1", ChatRole::User, "hello");
        let refs = [&user];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(40);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "hello");
    }
}
