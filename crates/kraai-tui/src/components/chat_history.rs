#![expect(
    clippy::expect_used,
    reason = "module-level regex constants are statically validated during development"
)]

use kraai_types::{ChatRole, Message};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};
use regex::Regex;
use std::sync::{Arc, LazyLock};

use super::{display_width, fitting_prefix, normalize_terminal_text};

mod markdown;

static TOOL_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<tool_call\b[^>]*>\s*\n?(.*?)</tool_call>").expect("valid regex")
});
const MESSAGE_GUTTER_WIDTH: usize = 3;

pub struct ChatHistory<'a> {
    messages: &'a [&'a Message],
    scroll: u16,
    auto_scroll: bool,
}

pub(crate) struct RenderedLine {
    spans: Vec<RenderedSpan>,
    bg: Option<Color>,
}

#[derive(Clone)]
struct RenderedSpan {
    text: String,
    style: Style,
}

impl<'a> ChatHistory<'a> {
    #[cfg(test)]
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
            let mut remaining = source_line;
            if remaining.is_empty() {
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
                let prefix_width = display_width(prefix);
                let available = width.saturating_sub(prefix_width);

                if available == 0 {
                    wrapped.push(Self::fit_to_width(prefix, width));
                    break;
                }

                let (chunk, _) = fitting_prefix(remaining, available);
                if chunk.is_empty() {
                    let Some(grapheme) =
                        unicode_segmentation::UnicodeSegmentation::graphemes(remaining, true)
                            .next()
                    else {
                        break;
                    };
                    wrapped.push(Self::fit_to_width(prefix, width));
                    remaining = &remaining[grapheme.len()..];
                } else {
                    wrapped.push(format!("{prefix}{chunk}"));
                    remaining = &remaining[chunk.len()..];
                }
                first_visual_line = false;

                if remaining.is_empty() {
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
        fitting_prefix(content, width).0.to_string()
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
            lines.push(Self::single_span_line(line, style));
        }
    }

    fn single_span_line(text: String, style: Style) -> RenderedLine {
        RenderedLine {
            spans: vec![RenderedSpan { text, style }],
            bg: style.bg,
        }
    }

    fn push_wrapped_spans(
        lines: &mut Vec<RenderedLine>,
        spans: &[RenderedSpan],
        width: usize,
        base_style: Style,
        first_prefix: &str,
        cont_prefix: &str,
    ) {
        if width == 0 {
            return;
        }

        let mut styled_graphemes = Vec::new();
        for span in spans {
            for grapheme in
                unicode_segmentation::UnicodeSegmentation::graphemes(span.text.as_str(), true)
            {
                styled_graphemes.push((grapheme, span.style));
            }
        }

        let mut idx = 0usize;
        let total = styled_graphemes.len();
        let mut first_visual_line = true;

        loop {
            if idx >= total && total > 0 {
                break;
            }

            let prefix = if first_visual_line {
                first_prefix
            } else {
                cont_prefix
            };
            let prefix_width = display_width(prefix);
            let available = width.saturating_sub(prefix_width);

            let mut line_spans = Vec::new();
            if !prefix.is_empty() {
                line_spans.push(RenderedSpan {
                    text: prefix.to_string(),
                    style: base_style,
                });
            }

            if available == 0 {
                lines.push(RenderedLine {
                    spans: line_spans,
                    bg: base_style.bg,
                });
                break;
            }

            let mut take_count = 0;
            let mut used_width = 0;
            while idx + take_count < total {
                let Some((grapheme, _)) = styled_graphemes.get(idx + take_count) else {
                    break;
                };
                let grapheme_width = display_width(grapheme);
                if used_width + grapheme_width > available {
                    break;
                }
                used_width += grapheme_width;
                take_count += 1;
            }

            if take_count > 0 {
                for (grapheme, style) in styled_graphemes
                    .get(idx..idx + take_count)
                    .unwrap_or_default()
                {
                    if let Some(last) = line_spans.last_mut()
                        && last.style == *style
                    {
                        last.text.push_str(grapheme);
                        continue;
                    }
                    line_spans.push(RenderedSpan {
                        text: (*grapheme).to_string(),
                        style: *style,
                    });
                }
                idx += take_count;
            } else if idx < total {
                // A grapheme wider than the remaining line cannot be rendered
                // without exceeding the viewport. Consume it while retaining
                // the fitting prefix so the visible view matches the buffer.
                idx += 1;
            }

            lines.push(RenderedLine {
                spans: line_spans,
                bg: base_style.bg,
            });

            first_visual_line = false;
            if total == 0 || idx >= total {
                break;
            }
        }
    }

    fn render_script_card(source: &str, width: usize) -> Vec<RenderedLine> {
        let mut lines = Vec::new();
        let header_style = Style::default()
            .fg(Color::Rgb(255, 200, 80))
            .add_modifier(Modifier::BOLD);
        let body_style = Style::default().fg(Color::Rgb(130, 230, 255));

        Self::push_wrapped_lines(&mut lines, "Nushell", width, header_style, "", "");
        Self::push_wrapped_lines(&mut lines, source, width, body_style, "  ", "  ");
        lines
    }

    fn render_assistant_message(content: &str, width: usize) -> Vec<RenderedLine> {
        let mut lines = Vec::new();
        let normal_style = Style::default().fg(Color::White);

        let mut cursor = 0usize;
        let mut found_tool_call = false;

        for caps in TOOL_CALL_RE.captures_iter(content) {
            let Some(full_match) = caps.get(0) else {
                continue;
            };
            found_tool_call = true;

            let before = &content[cursor..full_match.start()];
            if !before.trim().is_empty() {
                let mut before_lines = markdown::render_message(before, width, normal_style);
                lines.append(&mut before_lines);
            }

            if let Some(source) = caps.get(1).map(|m| m.as_str()) {
                let mut card_lines = Self::render_script_card(source, width);
                lines.append(&mut card_lines);
            }

            cursor = full_match.end();
        }

        if !found_tool_call {
            let mut parsed = markdown::render_message(content, width, normal_style);
            lines.append(&mut parsed);
        } else {
            let tail = &content[cursor..];
            if !tail.trim().is_empty() {
                let mut parsed = markdown::render_message(tail, width, normal_style);
                lines.append(&mut parsed);
            }
        }

        if lines.is_empty() {
            lines.push(Self::single_span_line(String::new(), normal_style));
        }

        lines
    }

    fn build_rendered_lines(&self, width: u16) -> Vec<RenderedLine> {
        Self::build_lines(self.messages, width)
    }

    pub(crate) fn separator_line() -> RenderedLine {
        Self::single_span_line(String::new(), Style::default())
    }

    pub(crate) fn line_text(line: &RenderedLine) -> String {
        let mut text = String::new();
        for span in &line.spans {
            text.push_str(&span.text);
        }
        text
    }

    fn gutter_prefix(marker: Option<char>) -> String {
        match marker {
            Some(marker) => format!(" {marker} "),
            None => "   ".to_string(),
        }
    }

    fn line_prefix_style(line: &RenderedLine) -> Style {
        match line.spans.first() {
            Some(span) => span.style,
            None => match line.bg {
                Some(bg) => Style::default().bg(bg),
                None => Style::default(),
            },
        }
    }

    fn add_message_gutter(lines: Vec<RenderedLine>, marker: char) -> Vec<RenderedLine> {
        if lines.is_empty() {
            return lines;
        }

        let marker_idx = lines
            .iter()
            .position(|line| !Self::line_text(line).is_empty())
            .unwrap_or(0);

        lines
            .into_iter()
            .enumerate()
            .map(|(idx, mut line)| {
                let prefix = if idx == marker_idx {
                    Self::gutter_prefix(Some(marker))
                } else {
                    Self::gutter_prefix(None)
                };
                let prefix_style = Self::line_prefix_style(&line);
                line.spans.insert(
                    0,
                    RenderedSpan {
                        text: prefix,
                        style: prefix_style,
                    },
                );
                line
            })
            .collect()
    }

    pub(crate) fn build_message_lines(msg: &Message, width: u16) -> Vec<RenderedLine> {
        let width = width as usize;
        if msg.role == ChatRole::System {
            return Vec::new();
        }

        let content = normalize_terminal_text(&msg.content);
        let content_width = width.saturating_sub(MESSAGE_GUTTER_WIDTH);
        match msg.role {
            ChatRole::User => {
                let user_style = Style::default()
                    .fg(Color::Rgb(255, 255, 255))
                    .bg(Color::DarkGray);

                let mut lines = vec![Self::single_span_line(String::new(), user_style)];

                for line in Self::wrap_with_prefix(&content, content_width, "", "") {
                    lines.push(Self::single_span_line(line, user_style));
                }

                lines.push(Self::single_span_line(String::new(), user_style));
                Self::add_message_gutter(lines, '❯')
            }
            ChatRole::Assistant => Self::add_message_gutter(
                Self::render_assistant_message(&content, content_width),
                '•',
            ),
            ChatRole::ToolCallResult => Self::add_message_gutter(
                Self::render_assistant_message(&content, content_width),
                '•',
            ),
            ChatRole::System => Vec::new(),
        }
    }

    pub(crate) fn build_lines(messages: &[&Message], width: u16) -> Vec<RenderedLine> {
        let mut rendered = Vec::new();
        for msg in messages {
            let mut message_lines = Self::build_message_lines(msg, width);
            if message_lines.is_empty() {
                continue;
            }

            if !rendered.is_empty() {
                rendered.push(Self::separator_line());
            }
            rendered.append(&mut message_lines);
        }
        rendered
    }

    pub(crate) fn render_prebuilt(
        lines: &[RenderedLine],
        area: Rect,
        buf: &mut Buffer,
        scroll: u16,
        auto_scroll: bool,
    ) {
        if area.width == 0 || area.height == 0 || lines.is_empty() {
            return;
        }

        let scroll = Self::resolve_scroll(lines.len() as u16, area.height, scroll, auto_scroll);

        let start_idx = scroll as usize;
        let end_idx = start_idx
            .saturating_add(area.height as usize)
            .min(lines.len());

        for (visual_idx, line) in lines
            .get(start_idx..end_idx)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let y = area.y + visual_idx as u16;
            Self::render_line(line, area, y, buf);
        }
    }

    pub(crate) fn render_prebuilt_sections(
        sections: &[Arc<Vec<RenderedLine>>],
        total_lines: u16,
        area: Rect,
        buf: &mut Buffer,
        scroll: u16,
        auto_scroll: bool,
    ) {
        if area.width == 0 || area.height == 0 || total_lines == 0 {
            return;
        }

        let scroll = Self::resolve_scroll(total_lines, area.height, scroll, auto_scroll);

        let start_idx = scroll as usize;
        let mut consumed = 0usize;
        let mut visual_idx = 0usize;

        for section in sections {
            if visual_idx >= area.height as usize {
                break;
            }

            let section_len = section.len();
            if consumed + section_len <= start_idx {
                consumed += section_len;
                continue;
            }

            let local_start = start_idx.saturating_sub(consumed);
            for line in section.iter().skip(local_start) {
                if visual_idx >= area.height as usize {
                    break;
                }

                let y = area.y + visual_idx as u16;
                Self::render_line(line, area, y, buf);

                visual_idx += 1;
            }
            consumed += section_len;
        }
    }

    pub(crate) fn resolve_scroll(
        total_lines: u16,
        viewport_height: u16,
        scroll: u16,
        auto_scroll: bool,
    ) -> u16 {
        let max_scroll = total_lines.saturating_sub(viewport_height);
        if auto_scroll {
            max_scroll
        } else {
            scroll.min(max_scroll)
        }
    }

    fn render_line(line: &RenderedLine, area: Rect, y: u16, buf: &mut Buffer) {
        let row_style = match line.bg {
            Some(bg) => Style::default().bg(bg),
            None => Style::default(),
        };
        for x in area.x..area.right() {
            buf[(x, y)].set_char(' ').set_style(row_style);
        }

        let mut x = area.x;
        for span in &line.spans {
            if x >= area.right() {
                break;
            }
            let remaining_width = area.right().saturating_sub(x) as usize;
            let (next_x, _) = buf.set_stringn(
                x,
                y,
                normalize_terminal_text(&span.text),
                remaining_width,
                span.style,
            );
            x = next_x;
        }
    }
}

impl<'a> Widget for ChatHistory<'a> {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        let lines = self.build_rendered_lines(area.width);
        Self::render_prebuilt(&lines, area, buf, self.scroll, self.auto_scroll);
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "render tests directly inspect expected visual lines"
)]
mod tests {
    use super::*;
    use kraai_types::{MessageId, MessageStatus};

    fn message(id: &str, role: ChatRole, content: &str) -> Message {
        Message {
            id: MessageId::new(id),
            parent_id: None,
            role,
            content: content.to_string(),
            status: MessageStatus::Complete,
            agent_profile_id: None,
            generation: None,
        }
    }

    #[test]
    fn wraps_unicode_without_panicking() {
        let wrapped = ChatHistory::wrap_with_prefix("你好你好", 4, "", "");
        assert_eq!(wrapped, ["你好", "你好"]);
        assert!(wrapped.iter().all(|line| display_width(line) <= 4));
    }

    #[test]
    fn wraps_wide_assistant_content_without_omitting_graphemes() {
        let assistant = message("1", ChatRole::Assistant, "你好你好");
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let rendered = history
            .build_rendered_lines(7)
            .iter()
            .map(ChatHistory::line_text)
            .collect::<Vec<_>>();

        assert_eq!(rendered, [" • 你好", "   你好"]);

        let area = Rect::new(0, 0, 7, 2);
        let mut buffer = Buffer::empty(area);
        history.render(area, &mut buffer);
        assert_eq!(buffer[(3, 0)].symbol(), "你");
        assert_eq!(buffer[(5, 0)].symbol(), "好");
        assert_eq!(buffer[(3, 1)].symbol(), "你");
        assert_eq!(buffer[(5, 1)].symbol(), "好");
    }

    #[test]
    fn keeps_wide_markdown_graphemes_within_narrow_chat_width() {
        let assistant = message("1", ChatRole::Assistant, "> 你");
        let lines = ChatHistory::build_message_lines(&assistant, 6);

        assert!(
            lines
                .iter()
                .all(|line| display_width(&ChatHistory::line_text(line)) <= 6)
        );
    }

    #[test]
    fn normalizes_control_characters_before_rendering_chat_history() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "\tlet value = 1;\n\u{1b}[31mred\u{7}",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(80);
        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

        assert!(
            rendered
                .iter()
                .any(|line| line.contains("    let value = 1;"))
        );
        assert!(rendered.iter().any(|line| line.contains("[31mred")));
        assert!(
            rendered
                .iter()
                .all(|line| !line.chars().any(char::is_control))
        );

        let area = Rect::new(0, 0, 80, 4);
        let mut buffer = Buffer::empty(area);
        history.render(area, &mut buffer);
        assert!(
            buffer
                .content()
                .iter()
                .all(|cell| !cell.symbol().chars().any(char::is_control))
        );
    }

    #[test]
    fn filters_system_messages_from_rendered_lines() {
        let system = message("1", ChatRole::System, "internal");
        let assistant = message("2", ChatRole::Assistant, "visible");
        let refs = [&system, &assistant];

        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(40);

        assert_eq!(lines.len(), 1);
        assert_eq!(ChatHistory::line_text(&lines[0]), " • visible");
    }

    #[test]
    fn renders_assistant_script_call_in_pretty_format() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "<tool_call timeout=\"10sec\" permissions=\"workspace-read\">\nopen /tmp/a.txt | lines | first 10\n</tool_call>",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);

        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| *line == " • Nushell"));
        assert!(
            rendered
                .iter()
                .any(|line| { line.contains("open /tmp/a.txt | lines | first 10") })
        );
        assert!(!rendered.iter().any(|line| line.contains("<tool_call>")));
    }

    #[test]
    fn renders_mixed_assistant_text_and_script_call() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "before\n<tool_call timeout=\"1sec\">\nls\n</tool_call>\nafter",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);
        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| *line == " • before"));
        assert!(rendered.iter().any(|line| *line == "   Nushell"));
        assert!(rendered.iter().any(|line| *line == "   after"));
    }

    #[test]
    fn renders_script_source_without_parsing_nushell() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "<tool_call timeout=\"1sec\">\nthis is not parsed here\n</tool_call>",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);
        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

        assert!(
            rendered
                .iter()
                .any(|line| line.contains("this is not parsed here"))
        );
    }

    #[test]
    fn renders_user_messages_with_gutter_indicator() {
        let user = message("1", ChatRole::User, "hello");
        let refs = [&user];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(40);

        assert_eq!(lines.len(), 3);
        assert_eq!(ChatHistory::line_text(&lines[0]), "   ");
        assert_eq!(ChatHistory::line_text(&lines[1]), " ❯ hello");
        assert_eq!(ChatHistory::line_text(&lines[2]), "   ");
    }

    #[test]
    fn renders_basic_markdown_blocks_for_assistant_messages() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "# Title\n- **one**\n1. [two](https://example.com)\n> quote\n`inline`",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);
        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| *line == " • Title"));
        assert!(rendered.iter().any(|line| *line == "   • one"));
        assert!(
            rendered
                .iter()
                .any(|line| *line == "   1. two (https://example.com)")
        );
        assert!(rendered.iter().any(|line| *line == "   │ quote"));
        assert!(rendered.iter().any(|line| *line == "   inline"));
    }

    #[test]
    fn renders_fenced_code_block_with_label() {
        let assistant = message("1", ChatRole::Assistant, "```rust\nfn main() {}\n```");
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);
        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| *line == " • [code: rust]"));
        assert!(rendered.iter().any(|line| *line == "     fn main() {}"));
    }

    #[test]
    fn renders_inline_code_with_distinct_color() {
        let assistant = message("1", ChatRole::Assistant, "alpha `beta` gamma");
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);

        assert_eq!(lines.len(), 1);
        assert_eq!(ChatHistory::line_text(&lines[0]), " • alpha beta gamma");

        let has_colored_inline_code = lines[0]
            .spans
            .iter()
            .any(|span| span.text == "beta" && span.style.fg == Some(Color::Rgb(255, 180, 90)));
        assert!(has_colored_inline_code);
    }

    #[test]
    fn wraps_assistant_messages_with_single_first_line_indicator() {
        let assistant = message("1", ChatRole::Assistant, "abcdefghijk");
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(8);
        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

        assert_eq!(rendered, vec![" • abcde", "   fghij", "   k"]);
    }

    #[test]
    fn skips_leading_blank_line_before_assistant_script_call() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "<tool_call timeout=\"5sec\">\nkraai-open-files /tmp/a.txt\n</tool_call>",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);
        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

        assert_eq!(rendered.first().map(String::as_str), Some(" • Nushell"));
    }

    #[test]
    fn skips_whitespace_only_tail_after_assistant_script_call() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "<tool_call timeout=\"5sec\">\nkraai-open-files /tmp/a.txt\n</tool_call>\n       \n\n          \n          ",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);
        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

        assert_eq!(
            rendered,
            vec![" • Nushell", "     kraai-open-files /tmp/a.txt"]
        );
    }
}
