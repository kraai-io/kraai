use kraai_types::{ChatRole, Message, MessageStatus, ScriptCall};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};
use std::sync::Arc;

use super::{display_width, fitting_prefix, normalize_terminal_text};

mod markdown;
mod scripts;
mod wrapping;

pub(crate) use scripts::{
    RunningCard, ScriptCard, ScriptMark, ScriptSection, ScriptView, running_dot,
};
pub(crate) use wrapping::wrap_words;

const MESSAGE_GUTTER_WIDTH: usize = 3;

pub struct ChatHistory<'a> {
    messages: &'a [&'a Message],
    scroll: usize,
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
    pub fn new(messages: &'a [&'a Message], scroll: usize, auto_scroll: bool) -> Self {
        Self {
            messages,
            scroll,
            auto_scroll,
        }
    }

    pub(crate) fn wrap_with_prefix(
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

        let source_lines = text.lines().chain(text.is_empty().then_some(""));

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

        let mut styled_graphemes = spans
            .iter()
            .flat_map(|span| {
                unicode_segmentation::UnicodeSegmentation::graphemes(span.text.as_str(), true)
                    .map(move |grapheme| (grapheme, span.style))
            })
            .peekable();
        let mut first_visual_line = true;

        loop {
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

            let mut consumed = false;
            let mut used_width = 0;
            while let Some(&(grapheme, style)) = styled_graphemes.peek() {
                let grapheme_width = display_width(grapheme);
                if used_width + grapheme_width > available {
                    break;
                }
                used_width += grapheme_width;
                styled_graphemes.next();
                consumed = true;
                if let Some(last) = line_spans.last_mut()
                    && last.style == style
                {
                    last.text.push_str(grapheme);
                    continue;
                }
                line_spans.push(RenderedSpan {
                    text: grapheme.to_string(),
                    style,
                });
            }

            if !consumed {
                // A grapheme wider than the remaining line cannot be rendered
                // without exceeding the viewport. Consume it while retaining
                // the fitting prefix so the visible view matches the buffer.
                styled_graphemes.next();
            }

            lines.push(RenderedLine {
                spans: line_spans,
                bg: base_style.bg,
            });

            first_visual_line = false;
            if styled_graphemes.peek().is_none() {
                break;
            }
        }
    }

    fn render_assistant_message(
        items: &[kraai_types::AssistantItem],
        width: usize,
        card: &dyn Fn(&str) -> ScriptCard,
    ) -> Vec<RenderedLine> {
        let mut lines = Vec::new();
        let style = Style::default().fg(Color::White);
        let mut marker = Some('•');
        for item in items {
            let block = match item {
                kraai_types::AssistantItem::Reasoning { .. } => continue,
                kraai_types::AssistantItem::Text { text, .. } => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    Self::add_message_gutter(
                        markdown::render_message(
                            &normalize_terminal_text(text),
                            width.saturating_sub(MESSAGE_GUTTER_WIDTH),
                            style,
                        ),
                        marker,
                    )
                }
                kraai_types::AssistantItem::ScriptCall(ScriptCall { call_id, input, .. }) => {
                    Self::build_script_card_lines(input, &card(call_id.as_str()), width)
                }
            };
            if block.is_empty() {
                continue;
            }
            if !lines.is_empty() {
                lines.push(Self::single_span_line(Self::gutter_prefix(None), style));
            }
            lines.extend(block);
            marker = None;
        }
        if lines.is_empty() {
            lines.push(Self::single_span_line(Self::gutter_prefix(marker), style));
        }
        lines
    }

    fn build_rendered_lines(&self, width: u16) -> Vec<RenderedLine> {
        Self::build_lines(self.messages, width, &|_| ScriptCard::Waiting)
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

    fn add_message_gutter(lines: Vec<RenderedLine>, marker: Option<char>) -> Vec<RenderedLine> {
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
                let prefix = Self::gutter_prefix(marker.filter(|_| idx == marker_idx));
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

    pub(crate) fn build_message_lines(
        msg: &Message,
        width: u16,
        card: &dyn Fn(&str) -> ScriptCard,
    ) -> Vec<RenderedLine> {
        let width = width as usize;
        if msg.role() == ChatRole::System {
            return Vec::new();
        }

        let display_text = msg.display_text();
        let content = normalize_terminal_text(&display_text);
        let content_width = width.saturating_sub(MESSAGE_GUTTER_WIDTH);
        match msg.role() {
            ChatRole::User => {
                let user_style = Style::default().fg(Color::Reset).bg(Color::DarkGray);

                let mut lines = vec![Self::single_span_line(String::new(), user_style)];

                for source in content.lines() {
                    wrapping::push_prose(
                        &mut lines,
                        &[RenderedSpan {
                            text: source.to_owned(),
                            style: user_style,
                        }],
                        width.saturating_sub(2),
                        user_style,
                    );
                }
                for line in lines.iter_mut().skip(1) {
                    line.spans.insert(
                        0,
                        RenderedSpan {
                            text: String::from(" "),
                            style: user_style,
                        },
                    );
                }

                lines.push(Self::single_span_line(String::new(), user_style));
                lines
            }
            ChatRole::Assistant => {
                let kraai_types::ConversationItem::Assistant { items } = &msg.content else {
                    return Vec::new();
                };
                let mut lines = Self::render_assistant_message(items, width, card);
                let label = match msg.status {
                    MessageStatus::Cancelled => Some("Cancelled"),
                    MessageStatus::Failed => Some("Interrupted by an error"),
                    MessageStatus::Complete | MessageStatus::Streaming { .. } => None,
                };
                if let Some(label) = label {
                    lines.push(Self::single_span_line(
                        Self::fit_to_width(&format!("{}{label}", Self::gutter_prefix(None)), width),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                lines
            }
            ChatRole::ToolCallResult => Self::add_message_gutter(
                markdown::render_message(
                    &content,
                    content_width,
                    Style::default().fg(Color::White),
                ),
                Some('•'),
            ),
            ChatRole::System => Vec::new(),
        }
    }

    pub(crate) fn build_lines(
        messages: &[&Message],
        width: u16,
        card: &dyn Fn(&str) -> ScriptCard,
    ) -> Vec<RenderedLine> {
        let mut rendered = Vec::new();
        for msg in messages {
            let mut message_lines = Self::build_message_lines(msg, width, card);
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
        scroll: usize,
        auto_scroll: bool,
    ) {
        if area.width == 0 || area.height == 0 || lines.is_empty() {
            return;
        }

        let scroll = Self::resolve_scroll(lines.len(), area.height, scroll, auto_scroll);

        let start_idx = scroll;
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
        total_lines: usize,
        area: Rect,
        buf: &mut Buffer,
        scroll: usize,
        auto_scroll: bool,
    ) {
        if area.width == 0 || area.height == 0 || total_lines == 0 {
            return;
        }

        let scroll = Self::resolve_scroll(total_lines, area.height, scroll, auto_scroll);

        let start_idx = scroll;
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
        total_lines: usize,
        viewport_height: u16,
        scroll: usize,
        auto_scroll: bool,
    ) -> usize {
        let max_scroll = total_lines.saturating_sub(usize::from(viewport_height));
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
mod tests;
