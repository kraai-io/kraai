use std::borrow::Cow;

use super::{display_width, normalize_terminal_text, normalized_byte_len};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};
use unicode_segmentation::UnicodeSegmentation;

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

const INPUT_STYLE: Style = Style::new().fg(Color::Reset).bg(Color::DarkGray);

impl<'a> TextInput<'a> {
    pub fn new(input: &'a str, cursor: usize) -> Self {
        Self {
            input: normalize_terminal_text(input),
            cursor: normalized_cursor(input, cursor),
        }
    }

    #[cfg(test)]
    fn wrap_text(content: &str, max_width: usize) -> Vec<String> {
        Self::wrap_segments(content, max_width)
            .into_iter()
            .map(|segment| segment.text(content).to_string())
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
            .find(|(_, segment)| segment.contains_cursor(safe_cursor))
            .map(|(index, segment)| {
                let column = display_width(
                    &normalized_input[segment.start..safe_cursor.min(segment.rendered_end)],
                );
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
                start: 0,
                end: 0,
                rendered_end: 0,
            }];
        }

        let mut wrapped = Vec::new();
        let mut line_start = 0usize;

        loop {
            let next_newline = content[line_start..].find('\n').map(|idx| line_start + idx);
            let line_end = next_newline.unwrap_or(content.len());
            let source_line = &content[line_start..line_end];
            let available = max_width;

            if source_line.is_empty() {
                wrapped.push(WrappedSegment {
                    start: line_start,
                    end: line_start,
                    rendered_end: line_start,
                });
            } else {
                let mut segment_start = line_start;
                let mut segment_width = 0usize;
                let mut word_boundary = None;
                for (offset, grapheme) in source_line.grapheme_indices(true) {
                    let grapheme_width = display_width(grapheme);
                    let grapheme_start = line_start + offset;
                    if grapheme.chars().all(char::is_whitespace) {
                        word_boundary = Some(grapheme_start + grapheme.len());
                    }
                    if grapheme_width > available {
                        if segment_start < grapheme_start {
                            wrapped.push(wrapped_segment(
                                segment_start,
                                grapheme_start,
                                grapheme_start,
                            ));
                        }

                        let segment_end = grapheme_start + grapheme.len();
                        wrapped.push(wrapped_segment(grapheme_start, segment_end, grapheme_start));
                        segment_start = segment_end;
                        segment_width = 0;
                        word_boundary = None;
                        continue;
                    }

                    if segment_width > 0 && segment_width + grapheme_width > available {
                        let segment_end = word_boundary
                            .filter(|boundary| *boundary > segment_start)
                            .unwrap_or(grapheme_start);
                        let rendered_end =
                            segment_start + content[segment_start..segment_end].trim_end().len();
                        wrapped.push(wrapped_segment(segment_start, segment_end, rendered_end));
                        segment_start = segment_end;
                        segment_width = display_width(
                            &content[segment_start..grapheme_start.max(segment_start)],
                        );
                        word_boundary = None;
                        if segment_start > grapheme_start {
                            continue;
                        }
                        if segment_width + grapheme_width > available {
                            wrapped.push(wrapped_segment(
                                segment_start,
                                grapheme_start,
                                grapheme_start,
                            ));
                            segment_start = grapheme_start;
                            segment_width = 0;
                        }
                    }
                    segment_width += grapheme_width;
                }

                if segment_start < line_end
                    || wrapped.last().is_some_and(|segment| {
                        segment.end == line_end
                            && segment.rendered_end > segment.start
                            && segment.rendered_end < segment.end
                    })
                {
                    wrapped.push(wrapped_segment(segment_start, line_end, line_end));
                }
            }

            let Some(newline_index) = next_newline else {
                break;
            };
            line_start = newline_index + 1;

            if line_start > content.len() {
                break;
            }
        }

        if wrapped.is_empty() {
            wrapped.push(WrappedSegment {
                start: 0,
                end: 0,
                rendered_end: 0,
            });
        }

        wrapped
    }

    pub fn get_height(&self, max_width: u16) -> u16 {
        let content_width = max_width.saturating_sub(H_PADDING * 2) as usize;
        (Self::wrap_segments(&self.input, content_width)
            .len()
            .max(1)
            .min(u16::MAX as usize) as u16)
            .saturating_add(V_PADDING * 2)
    }

    fn viewport(&self, area: Rect) -> (Vec<WrappedSegment>, usize, usize, usize) {
        let width = area.width.saturating_sub(H_PADDING * 2) as usize;
        let segments = Self::wrap_segments(&self.input, width);
        let row = segments
            .iter()
            .position(|segment| segment.contains_cursor(self.cursor))
            .unwrap_or(0);
        let column = segments
            .get(row)
            .map(|segment| {
                display_width(
                    self.input
                        .get(segment.start..self.cursor.min(segment.rendered_end))
                        .unwrap_or_default(),
                )
            })
            .unwrap_or(0);
        let visible = area.height.saturating_sub(V_PADDING * 2).max(1) as usize;
        let offset = row.saturating_sub(visible.saturating_sub(1));
        (segments, offset, row, column)
    }

    pub fn get_cursor_position(&self, area: Rect) -> (u16, u16) {
        let (_, offset, row, column) = self.viewport(area);
        (
            area.x
                .saturating_add(H_PADDING)
                .saturating_add(column.min(u16::MAX as usize) as u16)
                .min(area.right().saturating_sub(1)),
            area.y
                .saturating_add(V_PADDING.min(area.height.saturating_sub(1)))
                .saturating_add((row - offset) as u16)
                .min(area.bottom().saturating_sub(1)),
        )
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

        if area.width <= H_PADDING * 2 || area.height == 0 {
            return;
        }
        let (lines, offset, _, _) = self.viewport(area);
        let visible = area.height.saturating_sub(V_PADDING * 2).max(1) as usize;
        for (i, segment) in lines.iter().skip(offset).take(visible).enumerate() {
            let y = area.y + V_PADDING.min(area.height.saturating_sub(1)) + i as u16;
            if y < area.y + area.height {
                buf.set_stringn(
                    area.x + H_PADDING,
                    y,
                    segment.text(&self.input),
                    area.width.saturating_sub(H_PADDING * 2) as usize,
                    INPUT_STYLE,
                );
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WrappedSegment {
    start: usize,
    end: usize,
    rendered_end: usize,
}

impl WrappedSegment {
    fn contains_cursor(&self, cursor: usize) -> bool {
        cursor >= self.start
            && (cursor < self.end
                || cursor == self.end
                    && (self.rendered_end == self.end || self.rendered_end == self.start))
    }
    fn text<'b>(&self, content: &'b str) -> &'b str {
        &content[self.start..self.rendered_end]
    }
}

fn wrapped_segment(start: usize, end: usize, rendered_end: usize) -> WrappedSegment {
    WrappedSegment {
        start,
        end,
        rendered_end,
    }
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
    for (idx, grapheme) in input[segment.start..segment.rendered_end].grapheme_indices(true) {
        let grapheme_width = display_width(grapheme);
        if width + grapheme_width > column {
            return segment.start + idx;
        }
        width += grapheme_width;
        if width == column {
            return segment.start + idx + grapheme.len();
        }
    }
    segment.rendered_end
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
    fn word_wrapping_keeps_cursor_and_navigation_on_the_visible_word() {
        let input = "one distinct word";
        assert_eq!(TextInput::wrap_text(input, 10), ["one", "distinct", "word"]);
        let area = Rect::new(0, 0, 12, 5);
        assert_eq!(TextInput::new(input, 4).get_cursor_position(area), (1, 2));
        assert_eq!(TextInput::new(input, 3).get_cursor_position(area), (4, 1));
        let navigation = TextInput::cursor_navigation(input, 5, 12);
        assert_eq!(navigation.cursor_above, 1);
        assert_eq!(navigation.cursor_below, 14);
        assert_eq!(TextInput::wrap_text("hello world", 5), ["hello", "world"]);
        assert_eq!(TextInput::wrap_text("hello ", 5), ["hello", ""]);
        assert_eq!(
            TextInput::new("hello ", 6).get_cursor_position(Rect::new(0, 0, 7, 4)),
            (1, 2)
        );
    }

    #[test]
    fn vertical_navigation_clamps_to_visible_ends_of_shorter_wrapped_rows() {
        let input = "one distinct word another";
        let area = Rect::new(0, 0, 12, 8);
        let navigation = TextInput::cursor_navigation(input, 12, area.width);
        assert_eq!(navigation.cursor_above, 3);
        assert_eq!(navigation.cursor_below, 17);
        assert_eq!(
            TextInput::new(input, navigation.cursor_above).get_cursor_position(area),
            (4, 1)
        );
        assert_eq!(
            TextInput::new(input, navigation.cursor_below).get_cursor_position(area),
            (5, 3)
        );
    }

    #[test]
    fn long_words_with_wide_graphemes_stay_within_the_input_width() {
        for width in [2, 80] {
            let input = format!(" {}你", "a".repeat(width - 1));
            let rows = TextInput::wrap_text(&input, width);
            assert!(rows.iter().all(|row| display_width(row) <= width));
            assert_eq!(rows.concat(), input.trim_start());
        }
    }

    #[test]
    fn wraps_wide_graphemes_without_rendering_truncation() {
        let input = "你好你好";
        assert_eq!(TextInput::wrap_text(input, 6), vec!["你好你", "好"]);

        let area = Rect::new(0, 0, 8, 4);
        let mut buffer = Buffer::empty(area);
        TextInput::new(input, input.len()).render(area, &mut buffer);

        assert_eq!(buffer[(1, 2)].symbol(), "好");
        assert_eq!(
            TextInput::new(input, input.len()).get_cursor_position(area),
            (3, 2)
        );
    }

    #[test]
    fn keeps_an_overwide_first_grapheme_inside_the_input_width() {
        let input = "你";
        assert_eq!(TextInput::wrap_text(input, 1), vec![""]);

        let area = Rect::new(0, 0, 3, 3);
        let cursor = TextInput::new(input, input.len()).get_cursor_position(area);
        assert!(cursor.0 < area.right());
    }

    #[test]
    fn vertical_navigation_uses_display_columns_for_wide_graphemes() {
        let input = "你好你好";
        let navigation = TextInput::cursor_navigation(input, "你好你".len(), 8);

        assert!(navigation.can_move_down);
        assert_eq!(navigation.cursor_below, input.len());
    }

    #[test]
    fn multiline_layout_preserves_blank_lines_and_cursor_viewport() {
        let input = "a\n\n你e\u{301}👩‍💻\n";
        assert_eq!(
            TextInput::wrap_text(input, 4),
            ["a", "", "你e\u{301}", "👩‍💻", ""],
        );
        let area = Rect::new(2, 3, 6, 4);
        let widget = TextInput::new(input, input.len());
        assert_eq!(widget.get_height(area.width), 7);
        assert_eq!(widget.get_cursor_position(area), (3, 5));
        let mut buffer = Buffer::empty(area);
        widget.render(area, &mut buffer);
        assert_eq!(buffer[(3, 4)].symbol(), "👩‍💻");
        assert_eq!(buffer[(3, 5)].symbol(), " ");
    }

    #[test]
    fn narrow_layout_skips_overwide_graphemes() {
        let input = "你a你b";
        assert_eq!(TextInput::wrap_text(input, 0), [""]);
        assert_eq!(TextInput::wrap_text(input, 1), ["", "a", "", "b"]);
        assert_eq!(TextInput::wrap_text(input, 2), ["你", "a", "你", "b"]);
        assert_eq!(TextInput::wrap_text(input, 3), ["你a", "你b"]);
        assert_eq!(TextInput::wrap_text("\na\n", 1), ["", "a", ""]);
    }
}
