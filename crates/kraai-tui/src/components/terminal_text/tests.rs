use super::{display_width, fitting_prefix, normalize_terminal_text};

#[test]
fn expands_tabs_and_removes_other_control_characters() {
    assert_eq!(
        normalize_terminal_text("one\ttwo\nthree\u{1b}[31m\u{7}"),
        "one    two\nthree[31m"
    );
}

#[test]
fn measures_terminal_cells_at_grapheme_boundaries() {
    assert_eq!(display_width("a你👋"), 5);
    assert_eq!(fitting_prefix("a你b", 3), ("a你", 3));
}
