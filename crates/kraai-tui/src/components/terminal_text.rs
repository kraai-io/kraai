use std::borrow::Cow;

const TAB_SPACES: &str = "    ";

pub(crate) fn normalized_byte_len(ch: char) -> usize {
    match ch {
        '\t' => TAB_SPACES.len(),
        '\n' => ch.len_utf8(),
        ch if ch.is_control() => 0,
        ch => ch.len_utf8(),
    }
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
    use super::normalize_terminal_text;

    #[test]
    fn expands_tabs_and_removes_other_control_characters() {
        assert_eq!(
            normalize_terminal_text("one\ttwo\nthree\u{1b}[31m\u{7}"),
            "one    two\nthree[31m"
        );
    }
}
