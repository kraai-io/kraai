use kraai_types::{ChatRole, Message};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};
use regex::Regex;
use serde_json::{Map, Value};
use std::sync::{Arc, LazyLock};

static IMAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[([^\]]*)\]\(([^)]+)\)").expect("valid regex"));
static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").expect("valid regex"));
static STRONG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*([^*]+)\*\*|__([^_]+)__").expect("valid regex"));
static EMPHASIS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*([^*]+)\*|_([^_]+)_").expect("valid regex"));
static STRIKE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"~~([^~]+)~~").expect("valid regex"));
static ESCAPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\([\\`*_{}\[\]()#+.!~-])").expect("valid regex"));
static INLINE_CODE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`([^`]+)`").expect("valid regex"));
static HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(#{1,6})\s+(.*)$").expect("valid regex"));
static QUOTE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*>\s?(.*)$").expect("valid regex"));
static UNORDERED_LIST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[-*+]\s+(.*)$").expect("valid regex"));
static ORDERED_LIST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d+)\.\s+(.*)$").expect("valid regex"));
static FENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*```([A-Za-z0-9_-]+)?\s*$").expect("valid regex"));
static TOOL_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<tool_call>\s*\n?(.*?)</tool_call>").expect("valid regex"));
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VisibleChatLine {
    pub y: u16,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VisibleChatView {
    pub area: Rect,
    pub lines: Vec<VisibleChatLine>,
}

impl VisibleChatView {
    #[cfg(test)]
    pub(crate) fn from_strings(area: Rect, lines: &[&str]) -> Self {
        Self {
            area,
            lines: lines
                .iter()
                .enumerate()
                .map(|(idx, line)| VisibleChatLine {
                    y: area.y + idx as u16,
                    text: (*line).to_string(),
                })
                .collect(),
        }
    }
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
            lines.push(Self::single_span_line(line, style));
        }
    }

    fn single_span_line(text: String, style: Style) -> RenderedLine {
        RenderedLine {
            spans: vec![RenderedSpan { text, style }],
            bg: style.bg,
        }
    }

    fn strip_non_code_inline_markdown(text: &str) -> String {
        let text = IMAGE_RE.replace_all(text, "$1").to_string();
        let text = LINK_RE.replace_all(&text, "$1 ($2)").to_string();
        let text = STRONG_RE.replace_all(&text, "$1$2").to_string();
        let text = EMPHASIS_RE.replace_all(&text, "$1$2").to_string();
        let text = STRIKE_RE.replace_all(&text, "$1").to_string();
        ESCAPE_RE.replace_all(&text, "$1").to_string()
    }

    fn inline_markdown_spans(
        text: &str,
        base_style: Style,
        inline_code_style: Style,
    ) -> Vec<RenderedSpan> {
        let mut spans = Vec::new();
        let mut cursor = 0usize;

        for caps in INLINE_CODE_RE.captures_iter(text) {
            let full = match caps.get(0) {
                Some(m) => m,
                None => continue,
            };
            let before = &text[cursor..full.start()];
            let before_plain = Self::strip_non_code_inline_markdown(before);
            if !before_plain.is_empty() {
                spans.push(RenderedSpan {
                    text: before_plain,
                    style: base_style,
                });
            }

            if let Some(code) = caps.get(1).map(|m| m.as_str())
                && !code.is_empty()
            {
                spans.push(RenderedSpan {
                    text: code.to_string(),
                    style: inline_code_style,
                });
            }

            cursor = full.end();
        }

        let tail = &text[cursor..];
        let tail_plain = Self::strip_non_code_inline_markdown(tail);
        if !tail_plain.is_empty() {
            spans.push(RenderedSpan {
                text: tail_plain,
                style: base_style,
            });
        }

        if spans.is_empty() {
            spans.push(RenderedSpan {
                text: String::new(),
                style: base_style,
            });
        }

        spans
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

        let mut styled_chars = Vec::new();
        for span in spans {
            for ch in span.text.chars() {
                styled_chars.push((ch, span.style));
            }
        }

        let mut idx = 0usize;
        let total = styled_chars.len();
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
            let prefix_width = prefix.chars().count();
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
                first_visual_line = false;
                if total == 0 {
                    break;
                }
                continue;
            }

            let take_count = if total == 0 {
                0
            } else {
                available.min(total.saturating_sub(idx))
            };

            if take_count > 0 {
                for (ch, style) in &styled_chars[idx..idx + take_count] {
                    if let Some(last) = line_spans.last_mut()
                        && last.style == *style
                    {
                        last.text.push(*ch);
                        continue;
                    }
                    line_spans.push(RenderedSpan {
                        text: ch.to_string(),
                        style: *style,
                    });
                }
                idx += take_count;
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

    fn render_markdown_message(
        content: &str,
        width: usize,
        normal_style: Style,
    ) -> Vec<RenderedLine> {
        let mut lines = Vec::new();
        let heading_style = Style::default()
            .fg(Color::Rgb(255, 220, 120))
            .add_modifier(Modifier::BOLD);
        let quote_style = Style::default()
            .fg(Color::Rgb(170, 170, 170))
            .add_modifier(Modifier::ITALIC);
        let code_style = Style::default().fg(Color::Rgb(130, 230, 255));
        let inline_code_style = Style::default().fg(Color::Rgb(255, 180, 90));
        let list_style = normal_style;

        let mut in_fenced_code = false;
        let mut code_lang = String::new();

        for source_line in content.lines() {
            if let Some(caps) = FENCE_RE.captures(source_line) {
                if in_fenced_code {
                    in_fenced_code = false;
                    code_lang.clear();
                } else {
                    in_fenced_code = true;
                    code_lang = caps
                        .get(1)
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default();
                    if !code_lang.is_empty() {
                        Self::push_wrapped_lines(
                            &mut lines,
                            &format!("[code: {code_lang}]"),
                            width,
                            code_style,
                            "",
                            "",
                        );
                    }
                }
                continue;
            }

            if in_fenced_code {
                Self::push_wrapped_lines(&mut lines, source_line, width, code_style, "  ", "  ");
                continue;
            }

            if let Some(caps) = HEADING_RE.captures(source_line) {
                if let Some(text) = caps.get(2).map(|m| m.as_str()) {
                    let spans = Self::inline_markdown_spans(text, heading_style, inline_code_style);
                    Self::push_wrapped_spans(&mut lines, &spans, width, heading_style, "", "");
                }
                continue;
            }

            if let Some(caps) = QUOTE_RE.captures(source_line)
                && let Some(text) = caps.get(1).map(|m| m.as_str())
            {
                let spans = Self::inline_markdown_spans(text, quote_style, inline_code_style);
                Self::push_wrapped_spans(&mut lines, &spans, width, quote_style, "│ ", "│ ");
                continue;
            }

            if let Some(caps) = UNORDERED_LIST_RE.captures(source_line)
                && let Some(text) = caps.get(1).map(|m| m.as_str())
            {
                let spans = Self::inline_markdown_spans(text, list_style, inline_code_style);
                Self::push_wrapped_spans(&mut lines, &spans, width, list_style, "• ", "  ");
                continue;
            }

            if let Some(caps) = ORDERED_LIST_RE.captures(source_line) {
                let idx = caps.get(1).map(|m| m.as_str()).unwrap_or("1");
                if let Some(text) = caps.get(2).map(|m| m.as_str()) {
                    let spans = Self::inline_markdown_spans(text, list_style, inline_code_style);
                    let prefix = format!("{idx}. ");
                    Self::push_wrapped_spans(&mut lines, &spans, width, list_style, &prefix, "   ");
                    continue;
                }
            }

            let spans = Self::inline_markdown_spans(source_line, normal_style, inline_code_style);
            Self::push_wrapped_spans(&mut lines, &spans, width, normal_style, "", "");
        }

        if lines.is_empty() {
            lines.push(Self::single_span_line(String::new(), normal_style));
        }

        lines
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
            lines.push(Self::single_span_line(
                Self::fit_to_width("  (no args)", width),
                body_style,
            ));
        }

        Some(lines)
    }

    fn render_assistant_message(content: &str, width: usize) -> Vec<RenderedLine> {
        let mut lines = Vec::new();
        let normal_style = Style::default().fg(Color::White);

        let mut cursor = 0usize;
        let mut found_tool_call = false;

        for caps in TOOL_CALL_RE.captures_iter(content) {
            let full_match = match caps.get(0) {
                Some(m) => m,
                None => continue,
            };
            found_tool_call = true;

            let before = &content[cursor..full_match.start()];
            if !before.is_empty() {
                let mut before_lines = Self::render_markdown_message(before, width, normal_style);
                lines.append(&mut before_lines);
            }

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
            let mut parsed = Self::render_markdown_message(content, width, normal_style);
            lines.append(&mut parsed);
        } else {
            let tail = &content[cursor..];
            if !tail.is_empty() {
                let mut parsed = Self::render_markdown_message(tail, width, normal_style);
                lines.append(&mut parsed);
            }
        }

        if lines.is_empty() {
            lines.push(Self::single_span_line(String::new(), normal_style));
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
            && let Ok(value) = serde_json::from_str::<Value>(json)
        {
            return value.get("error").is_some();
        }

        false
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

        let content_width = width.saturating_sub(MESSAGE_GUTTER_WIDTH);
        match msg.role {
            ChatRole::User => {
                let user_style = Style::default()
                    .fg(Color::Rgb(255, 255, 255))
                    .bg(Color::DarkGray);

                let mut lines = vec![Self::single_span_line(String::new(), user_style)];

                for line in Self::wrap_with_prefix(&msg.content, content_width, "", "") {
                    lines.push(Self::single_span_line(line, user_style));
                }

                lines.push(Self::single_span_line(String::new(), user_style));
                Self::add_message_gutter(lines, '>')
            }
            ChatRole::Assistant => Self::add_message_gutter(
                Self::render_assistant_message(&msg.content, content_width),
                '•',
            ),
            ChatRole::Tool => {
                if !Self::should_render_tool_message(&msg.content) {
                    Vec::new()
                } else {
                    let mut lines = Vec::new();
                    for line in
                        Self::wrap_with_prefix(&msg.content, content_width, "tool: ", "      ")
                    {
                        lines.push(Self::single_span_line(
                            line,
                            Style::default().fg(Color::Yellow),
                        ));
                    }
                    Self::add_message_gutter(lines, '•')
                }
            }
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

        for (visual_idx, line) in lines[start_idx..end_idx].iter().enumerate() {
            let y = area.y + visual_idx as u16;
            let row_style = match line.bg {
                Some(bg) => Style::default().bg(bg),
                None => Style::default(),
            };

            for x_offset in 0..area.width {
                buf[(area.x + x_offset, y)]
                    .set_char(' ')
                    .set_style(row_style);
            }

            let mut char_idx = 0usize;
            'outer: for span in &line.spans {
                for ch in span.text.chars() {
                    let x = area.x + char_idx as u16;
                    if x >= area.x + area.width {
                        break 'outer;
                    }
                    buf[(x, y)].set_char(ch).set_style(span.style);
                    char_idx += 1;
                }
            }
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
                let row_style = match line.bg {
                    Some(bg) => Style::default().bg(bg),
                    None => Style::default(),
                };

                for x_offset in 0..area.width {
                    buf[(area.x + x_offset, y)]
                        .set_char(' ')
                        .set_style(row_style);
                }

                let mut char_idx = 0usize;
                'outer: for span in &line.spans {
                    for ch in span.text.chars() {
                        let x = area.x + char_idx as u16;
                        if x >= area.x + area.width {
                            break 'outer;
                        }
                        buf[(x, y)].set_char(ch).set_style(span.style);
                        char_idx += 1;
                    }
                }

                visual_idx += 1;
            }
            consumed += section_len;
        }
    }

    pub(crate) fn visible_view_from_sections(
        sections: &[Arc<Vec<RenderedLine>>],
        total_lines: u16,
        area: Rect,
        scroll: u16,
        auto_scroll: bool,
    ) -> VisibleChatView {
        if area.width == 0 || area.height == 0 || total_lines == 0 {
            return VisibleChatView {
                area,
                lines: Vec::new(),
            };
        }

        let scroll = Self::resolve_scroll(total_lines, area.height, scroll, auto_scroll);

        let start_idx = scroll as usize;
        let mut consumed = 0usize;
        let mut visual_idx = 0usize;
        let mut lines = Vec::new();

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

                lines.push(VisibleChatLine {
                    y: area.y + visual_idx as u16,
                    text: Self::line_text(line),
                });
                visual_idx += 1;
            }
            consumed += section_len;
        }

        VisibleChatView { area, lines }
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
            tool_state_snapshot: None,
            tool_state_deltas: Vec::new(),
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
        assert_eq!(ChatHistory::line_text(&lines[0]), " • visible");
    }

    #[test]
    fn renders_assistant_tool_call_in_pretty_format() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "<tool_call>\ntool: read_file\nfiles[1]: /tmp/a.txt\nmax_size: 10\n</tool_call>",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);

        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| *line == " • read_file"));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("   files[1]: /tmp/a.txt"))
        );
        assert!(rendered.iter().any(|line| line.contains("max_size: 10")));
        assert!(!rendered.iter().any(|line| line.contains("<tool_call>")));
    }

    #[test]
    fn renders_mixed_assistant_text_and_tool_call() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "before\n<tool_call>\ntool: read_file\nfiles[1]: /tmp/a.txt\n</tool_call>\nafter",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);
        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| *line == " • before"));
        assert!(rendered.iter().any(|line| *line == "   read_file"));
        assert!(rendered.iter().any(|line| *line == "   after"));
    }

    #[test]
    fn falls_back_to_raw_tool_call_block_on_parse_failure() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "<tool_call>\ntool read_file\nbad\n</tool_call>",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);
        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| line.contains(" • <tool_call>")));
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

        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();
        assert!(
            rendered
                .iter()
                .any(|line| line.contains(" • tool: Tool 'read_file' result:"))
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
        assert!(ChatHistory::line_text(&lines[0]).contains("denied by user"));
    }

    #[test]
    fn renders_user_messages_with_gutter_indicator() {
        let user = message("1", ChatRole::User, "hello");
        let refs = [&user];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(40);

        assert_eq!(lines.len(), 3);
        assert_eq!(ChatHistory::line_text(&lines[0]), "   ");
        assert_eq!(ChatHistory::line_text(&lines[1]), " > hello");
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
    fn skips_leading_blank_line_before_assistant_tool_call() {
        let assistant = message(
            "1",
            ChatRole::Assistant,
            "<tool_call>\ntool: read_file\nfiles[1]: /tmp/a.txt\n</tool_call>",
        );
        let refs = [&assistant];
        let history = ChatHistory::new(&refs, 0, true);
        let lines = history.build_rendered_lines(120);
        let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

        assert_eq!(rendered.first().map(String::as_str), Some(" • read_file"));
    }

    #[test]
    fn resolve_scroll_uses_bottom_when_auto_scroll_is_enabled() {
        assert_eq!(ChatHistory::resolve_scroll(20, 8, 0, true), 12);
    }

    #[test]
    fn resolve_scroll_clamps_manual_scroll_to_bottom() {
        assert_eq!(ChatHistory::resolve_scroll(20, 8, 99, false), 12);
    }
}
