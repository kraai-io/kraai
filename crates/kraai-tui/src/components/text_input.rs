use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{normalize_terminal_text, normalized_byte_len};

pub struct TextInput<'a> {
    input: Cow<'a, str>,
    cursor: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorNavigation {
    pub(crate) can_move_up: bool,
    pub(crate) can_move_down: bool,
    pub(crate) cursor_above: usize,
    pub(crate) cursor_below: usize,
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
        Self {
            input: normalize_terminal_text(input),
            cursor: normalized_cursor(input, cursor),
        }
    }

    fn wrap_text(content: &str, max_width: usize) -> Vec<String> {
        Self::wrap_segments(content, max_width)
            .into_iter()
            .map(|segment| segment.text)
            .collect()
    }

    pub(crate) fn cursor_navigation(
        input: &str,
        cursor: usize,
        max_width: u16,
    ) -> CursorNavigation {
        let max_width = max_width.saturating_sub(H_PADDING * 2) as usize;
        let normalized_input = normalize_terminal_text(input);
        let segments = Self::wrap_segments(&normalized_input, max_width);
        let safe_cursor = normalized_cursor(input, cursor);
        let current_line = segments
            .iter()
            .enumerate()
            .find(|(_, segment)| safe_cursor >= segment.start && safe_cursor <= segment.end)
            .map(|(index, segment)| {
                let column =
                    display_width(&normalized_input[segment.start..safe_cursor.min(segment.end)]);
                (index, column)
            })
            .unwrap_or((0, 0));

        let (line_index, column) = current_line;
        CursorNavigation {
            can_move_up: line_index > 0,
            can_move_down: line_index + 1 < segments.len(),
            cursor_above: source_cursor(
                input,
                line_cursor(
                    &normalized_input,
                    &segments,
                    line_index.saturating_sub(1),
                    column,
                ),
            ),
            cursor_below: source_cursor(
                input,
                line_cursor(&normalized_input, &segments, line_index + 1, column),
            ),
        }
    }

    fn wrap_segments(content: &str, max_width: usize) -> Vec<WrappedSegment> {
        if max_width == 0 {
            return vec![WrappedSegment {
                text: String::new(),
                start: 0,
                end: 0,
            }];
        }

        let mut wrapped = Vec::new();
        let mut line_start = 0usize;
        let mut source_index = 0usize;
        loop {
            let next_newline = content[line_start..].find('\n').map(|idx| line_start + idx);
            let line_end = next_newline.unwrap_or(content.len());
            let source_line = &content[line_start..line_end];
            let prefix = if source_index == 0 {
                PROMPT
            } else {
                CONTINUATION_PREFIX
            };
            let prefix_width = display_width(prefix);
            let available = max_width.saturating_sub(prefix_width);

            if source_line.is_empty() {
                wrapped.push(WrappedSegment {
                    text: prefix.to_string(),
                    start: line_start,
                    end: line_start,
                });
            } else if available == 0 {
                wrapped.push(WrappedSegment {
                    text: prefix.chars().take(max_width).collect(),
                    start: line_start,
                    end: line_start,
                });
            } else {
                let mut segment_start = line_start;
                let mut segment_width = 0usize;
                for (offset, grapheme) in source_line.grapheme_indices(true) {
                    let grapheme_width = grapheme.width();
                    if segment_width > 0 && segment_width + grapheme_width > available {
                        let segment_end = line_start + offset;
                        let line_prefix = if segment_start == line_start {
                            prefix
                        } else {
                            CONTINUATION_PREFIX
                        };
                        wrapped.push(WrappedSegment {
                            text: format!("{line_prefix}{}", &content[segment_start..segment_end]),
                            start: segment_start,
                            end: segment_end,
                        });
                        segment_start = segment_end;
                        segment_width = 0;
                    }
                    segment_width += grapheme_width;
                }

                let line_prefix = if segment_start == line_start {
                    prefix
                } else {
                    CONTINUATION_PREFIX
                };
                wrapped.push(WrappedSegment {
                    text: format!("{line_prefix}{}", &content[segment_start..line_end]),
                    start: segment_start,
                    end: line_end,
                });
            }

            let Some(newline_index) = next_newline else {
                break;
            };
            line_start = newline_index + 1;
            source_index += 1;
            if line_start > content.len() {
                break;
            }
        }

        if wrapped.is_empty() {
            wrapped.push(WrappedSegment {
                text: PROMPT.to_string(),
                start: 0,
                end: 0,
            });
        }

        wrapped
    }

    pub fn get_height(&self, max_width: u16) -> u16 {
        let content_width = max_width.saturating_sub(H_PADDING * 2) as usize;
        Self::wrap_text(&self.input, content_width).len().max(1) as u16 + (V_PADDING * 2)
    }

    pub fn get_cursor_position(&self, area: Rect) -> (u16, u16) {
        let safe_cursor = self
            .cursor
            .min(self.input.len())
            .min(next_char_boundary(&self.input, self.cursor));
        let max_width = area.width.saturating_sub(H_PADDING * 2) as usize;
        let lines = Self::wrap_text(&self.input[..safe_cursor], max_width);

        let line_count = lines.len();
        let empty = String::new();
        let last_line = lines.last().unwrap_or(&empty);

        let cursor_line_idx = (line_count.saturating_sub(1)) as u16;
        let cursor_x = area.x + H_PADDING + display_width(last_line) as u16;
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
        let lines = Self::wrap_text(&self.input, max_width);

        for (i, line) in lines.iter().enumerate() {
            let y = area.y + V_PADDING + i as u16;
            if y < area.y + area.height {
                buf.set_stringn(
                    area.x + H_PADDING,
                    y,
                    line,
                    area.width.saturating_sub(H_PADDING * 2) as usize,
                    INPUT_STYLE,
                );
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WrappedSegment {
    text: String,
    start: usize,
    end: usize,
}

fn line_cursor(
    input: &str,
    segments: &[WrappedSegment],
    line_index: usize,
    column: usize,
) -> usize {
    let Some(segment) = segments.get(line_index) else {
        return input.len();
    };

    let mut width = 0usize;
    for (idx, grapheme) in input[segment.start..segment.end].grapheme_indices(true) {
        let grapheme_width = grapheme.width();
        if width + grapheme_width > column {
            return segment.start + idx;
        }
        width += grapheme_width;
        if width == column {
            return segment.start + idx + grapheme.len();
        }
    }
    segment.end
}

fn display_width(text: &str) -> usize {
    text.width()
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

fn previous_char_boundary(s: &str, idx: usize) -> usize {
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

fn normalized_cursor(input: &str, cursor: usize) -> usize {
    let cursor = previous_char_boundary(input, cursor.min(input.len()));
    input[..cursor].chars().map(normalized_byte_len).sum()
}

fn source_cursor(input: &str, normalized_cursor: usize) -> usize {
    let mut normalized_offset = 0usize;
    for (source_offset, ch) in input.char_indices() {
        if normalized_cursor <= normalized_offset {
            return source_offset;
        }

        normalized_offset += normalized_byte_len(ch);
        if normalized_cursor <= normalized_offset {
            return source_offset + ch.len_utf8();
        }
    }
    input.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_wide_graphemes_without_rendering_truncation() {
        let input = "你好你好";
        assert_eq!(TextInput::wrap_text(input, 8), vec!["> 你好你", "  好"]);

        let area = Rect::new(0, 0, 10, 4);
        let mut buffer = Buffer::empty(area);
        TextInput::new(input, input.len()).render(area, &mut buffer);

        assert_eq!(buffer[(3, 2)].symbol(), "好");
        assert_eq!(
            TextInput::new(input, input.len()).get_cursor_position(area),
            (5, 2)
        );
    }

    #[test]
    fn vertical_navigation_uses_display_columns_for_wide_graphemes() {
        let input = "你好你好";
        let navigation = TextInput::cursor_navigation(input, "你好你".len(), 10);

        assert!(navigation.can_move_down);
        assert_eq!(navigation.cursor_below, input.len());
    }
}
