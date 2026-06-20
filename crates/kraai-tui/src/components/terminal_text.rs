use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const TAB_SPACES: &str = "    ";

pub(crate) fn normalized_byte_len(ch: char) -> usize {
    match ch {
        '\t' => TAB_SPACES.len(),
        '\n' => ch.len_utf8(),
        ch if ch.is_control() => 0,
        ch => ch.len_utf8(),
    }
}

/// Returns the number of terminal cells needed to render `text`.
pub(crate) fn display_width(text: &str) -> usize {
    text.width()
}

/// Returns the largest grapheme-aligned prefix that fits in `width` terminal
/// cells, together with its display width.
pub(crate) fn fitting_prefix(text: &str, width: usize) -> (&str, usize) {
    let mut end = 0;
    let mut used_width = 0;

    for (offset, grapheme) in text.grapheme_indices(true) {
        let grapheme_width = display_width(grapheme);
        if used_width + grapheme_width > width {
            break;
        }
        end = offset + grapheme.len();
        used_width += grapheme_width;
    }

    (&text[..end], used_width)
}

/// Returns every grapheme that overlaps the inclusive terminal-cell range.
pub(crate) fn text_in_cell_range(text: &str, start: usize, end: usize) -> String {
    let mut selected = String::new();
    let mut cell = 0;

    for grapheme in text.graphemes(true) {
        let grapheme_width = display_width(grapheme);
        let next_cell = cell + grapheme_width;
        if grapheme_width > 0 && cell <= end && next_cell > start {
            selected.push_str(grapheme);
        }
        cell = next_cell;
    }

    selected
}

/// Makes externally supplied text safe for terminal rendering.
///
/// Newlines retain their structural meaning, tabs become four spaces, and all
/// other control characters are removed before they can reach a terminal cell.
pub(crate) fn normalize_terminal_text(text: &str) -> Cow<'_, str> {
    if !text
        .chars()
        .any(|ch| ch == '\t' || (ch.is_control() && ch != '\n'))
    {
        return Cow::Borrowed(text);
    }

    let mut normalized = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => normalized.push(ch),
            '\t' => normalized.push_str(TAB_SPACES),
            _ if normalized_byte_len(ch) == 0 => {}
            ch => normalized.push(ch),
        }
    }
    Cow::Owned(normalized)
}

#[cfg(test)]
mod tests {
    use super::{display_width, fitting_prefix, normalize_terminal_text, text_in_cell_range};

    #[test]
    fn expands_tabs_and_removes_other_control_characters() {
        assert_eq!(
            normalize_terminal_text("one\ttwo\nthree\u{1b}[31m\u{7}"),
            "one    two\nthree[31m"
        );
    }

    #[test]
    fn measures_and_slices_terminal_cells_at_grapheme_boundaries() {
        assert_eq!(display_width("a你👋"), 5);
        assert_eq!(fitting_prefix("a你b", 3), ("a你", 3));
        assert_eq!(text_in_cell_range("a你b", 2, 2), "你");
    }
}
