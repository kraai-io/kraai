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
fn prefix_shifts_the_cursor_and_takes_the_accent_colour() {
    let area = Rect::new(0, 0, 20, 3);
    let widget = TextInput::with_prefix("/model ", "son", 3);
    assert_eq!(widget.get_cursor_position(area), (11, 1));
    let mut buffer = Buffer::empty(area);
    widget.render(area, &mut buffer);
    let row: String = (1..11).map(|x| buffer[(x, 1)].symbol()).collect();
    assert_eq!(row, "/model son");
    assert_eq!(buffer[(1, 1)].fg, crate::theme::ACCENT);
    assert_eq!(buffer[(7, 1)].fg, crate::theme::ACCENT);
    assert_eq!(buffer[(8, 1)].fg, Color::Reset);
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
