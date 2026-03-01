use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};
use regex::Regex;
use serde_json::{Map, Value};
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

    fn push_wrapped_lines(
        lines: &mut Vec<RenderedLine>,
        text: &str,
        width: usize,
        style: Style,
        first_prefix: &str,
        cont_prefix: &str,
    ) {
        if text.is_empty() {
            return;
        }

        for line in Self::wrap_with_prefix(text, width, first_prefix, cont_prefix) {
            lines.push(RenderedLine { text: line, style });
        }
    }

    fn parse_tool_call(toon_content: &str) -> Option<(String, Option<String>)> {
        let value: Value = toon_format::decode_default(toon_content).ok()?;
        let object = value.as_object()?;
        let tool_name = object.get("tool")?.as_str()?;

        let mut args: Map<String, Value> = Map::new();
        for (key, val) in object {
            if key != "tool" {
                args.insert(key.clone(), val.clone());
            }
        }

        let args_text = if args.is_empty() {
            None
        } else {
            Some(toon_format::encode_default(&Value::Object(args)).ok()?)
        };

        Some((tool_name.to_string(), args_text))
    }

    fn render_tool_call_card(toon_content: &str, width: usize) -> Option<Vec<RenderedLine>> {
        let (tool_name, args_text) = Self::parse_tool_call(toon_content)?;
        let mut lines = Vec::new();
        let header_style = Style::default()
            .fg(Color::Rgb(255, 200, 80))
            .add_modifier(Modifier::BOLD);
        let body_style = Style::default().fg(Color::Rgb(130, 230, 255));

        Self::push_wrapped_lines(&mut lines, &tool_name, width, header_style, "", "");

        if let Some(args_text) = args_text {
            Self::push_wrapped_lines(&mut lines, &args_text, width, body_style, "  ", "  ");
        } else {
            lines.push(RenderedLine {
                text: Self::fit_to_width("  (no args)", width),
                style: body_style,
            });
        }

        Some(lines)
    }

    fn render_assistant_message(content: &str, width: usize) -> Vec<RenderedLine> {
        let mut lines = Vec::new();
        let normal_style = Style::default().fg(Color::Green);

        let tool_call_re = Regex::new(r"(?s)```tool_call\s*\n(.*?)```").expect("valid regex");
        let mut cursor = 0usize;
        let mut found_tool_call = false;

        for caps in tool_call_re.captures_iter(content) {
            let full_match = match caps.get(0) {
                Some(m) => m,
                None => continue,
            };
            found_tool_call = true;

            let before = &content[cursor..full_match.start()];
            Self::push_wrapped_lines(&mut lines, before, width, normal_style, "", "");

            if let Some(raw_toon) = caps.get(1).map(|m| m.as_str()) {
                if let Some(mut card_lines) = Self::render_tool_call_card(raw_toon, width) {
                    lines.append(&mut card_lines);
                } else {
                    Self::push_wrapped_lines(
                        &mut lines,
                        full_match.as_str(),
                        width,
                        normal_style,
                        "",
                        "",
                    );
                }
            }

            cursor = full_match.end();
        }

        if !found_tool_call {
            Self::push_wrapped_lines(&mut lines, content, width, normal_style, "", "");
        } else {
            let tail = &content[cursor..];
            Self::push_wrapped_lines(&mut lines, tail, width, normal_style, "", "");
        }

        if lines.is_empty() {
            lines.push(RenderedLine {
                text: String::new(),
                style: normal_style,
            });
        }

        lines
    }

    fn should_render_tool_message(content: &str) -> bool {
        if content.contains("Failed to parse tool call:") {
            return true;
        }

        if content.contains("was denied by user") {
            return true;
        }

        if let Some((_, json)) = content.split_once("result:\n")
            && let Ok(value) = serde_json::from_str::<Value>(json) {
                return value.get("error").is_some();
            }

        false
    }

    fn build_rendered_lines(&self, width: u16) -> Vec<RenderedLine> {
        let width = width as usize;
        let mut rendered = Vec::new();

        for msg in self.messages {
            if msg.role == ChatRole::System {
                continue;
            }

            let mut message_lines = match msg.role {
                ChatRole::User => {
                    let user_style = Style::default()
                        .fg(Color::Rgb(255, 255, 255))
                        .bg(Color::DarkGray);

                    let mut lines = vec![RenderedLine {
                        text: String::new(),
                        style: user_style,
                    }];

                    for line in Self::wrap_with_prefix(&msg.content, width, "", "") {
                        lines.push(RenderedLine {
                            text: line,
                            style: user_style,
                        });
                    }

                    lines.push(RenderedLine {
                        text: String::new(),
                        style: user_style,
                    });
                    lines
                }
                ChatRole::Assistant => Self::render_assistant_message(&msg.content, width),
                ChatRole::Tool => {
                    if !Self::should_render_tool_message(&msg.content) {
                        Vec::new()
                    } else {
                        let mut lines = Vec::new();
                        for line in Self::wrap_with_prefix(&msg.content, width, "tool: ", "      ")
                        {
                            lines.push(RenderedLine {
                                text: line,
                                style: Style::default().fg(Color::Yellow),
                            });
                        }
                        lines
                    }
                }
                ChatRole::System => Vec::new(),
            };

            if message_lines.is_empty() {
                continue;
            }

            if !rendered.is_empty() {
                rendered.push(RenderedLine {
                    text: String::new(),
                    style: Style::default(),
                });
            }

            rendered.append(&mut message_lines);
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
            let row_style = match line.style.bg {
                Some(bg) => Style::default().bg(bg),
                None => Style::default(),
            };

            for x_offset in 0..area.width {
                buf[(area.x + x_offset, y)]
                    .set_char(' ')
                    .set_style(row_style);
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
    fn renders_assistant_tool_call_in_pretty_format() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "```tool_call\ntool: read_file\nfiles[1]: /tmp/a.txt\nmax_size: 10\n```",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);

        let rendered = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| *line == "read_file"));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("files[1]: /tmp/a.txt"))
        );
        assert!(rendered.iter().any(|line| line.contains("max_size: 10")));
        assert!(!rendered.iter().any(|line| line.contains("```tool_call")));
    }

    #[test]
    fn renders_mixed_assistant_text_and_tool_call() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "before\n```tool_call\ntool: read_file\nfiles[1]: /tmp/a.txt\n```\nafter",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);
        let rendered = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| *line == "before"));
        assert!(rendered.iter().any(|line| *line == "read_file"));
        assert!(rendered.iter().any(|line| *line == "after"));
    }

    #[test]
    fn falls_back_to_raw_tool_call_block_on_parse_failure() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "```tool_call\ntool read_file\nbad\n```",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);
        let rendered = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| line.contains("```tool_call")));
    }

    #[test]
    fn hides_successful_tool_messages() {
        let tool = message(
            "1",
            ChatRole::Tool,
            "Tool 'read_file' result:\n{\n  \"ok\": true\n}",
        );
        let refs = [&tool];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);

        assert!(lines.is_empty());
    }

    #[test]
    fn shows_tool_error_messages() {
        let tool = message(
            "1",
            ChatRole::Tool,
            "Tool 'read_file' result:\n{\n  \"error\": \"denied\"\n}",
        );
        let refs = [&tool];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);

        let rendered = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>();
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("tool: Tool 'read_file' result:"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("\"error\": \"denied\""))
        );
    }

    #[test]
    fn shows_denied_tool_messages() {
        let tool = message("1", ChatRole::Tool, "Tool 'read_file' was denied by user");
        let refs = [&tool];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);

        assert_eq!(lines.len(), 1);
        assert!(lines[0].text.contains("denied by user"));
    }

    #[test]
    fn renders_user_messages_without_prefix() {
        let user = message("1", ChatRole::User, "hello");
        let refs = [&user];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(40);

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "");
        assert_eq!(lines[1].text, "hello");
        assert_eq!(lines[2].text, "");
    }
}
