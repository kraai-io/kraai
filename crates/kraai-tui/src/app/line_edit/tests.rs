use super::*;

fn line(text: &str, cursor: usize) -> LineEdit {
    LineEdit {
        text: text.to_owned(),
        cursor,
    }
}

fn press(line: &mut LineEdit, code: KeyCode, modifiers: KeyModifiers) -> Option<LineKey> {
    line.handle_key(KeyEvent::new(code, modifiers))
}

#[test]
fn arrows_move_through_graphemes_and_typing_inserts_at_the_cursor() {
    let mut url = LineEdit::default();
    url.set("https://e\u{301}xample.com");
    for _ in 0.."xample.com".len() {
        press(&mut url, KeyCode::Left, KeyModifiers::NONE);
    }
    assert_eq!(url.cursor(), "https://e\u{301}".len());
    assert_eq!(
        press(&mut url, KeyCode::Left, KeyModifiers::NONE),
        Some(LineKey::Moved)
    );
    assert_eq!(url.cursor(), "https://".len());
    press(&mut url, KeyCode::Char('w'), KeyModifiers::NONE);
    assert_eq!(url.as_str(), "https://we\u{301}xample.com");
    press(&mut url, KeyCode::Delete, KeyModifiers::NONE);
    assert_eq!(url.as_str(), "https://wxample.com");
    press(&mut url, KeyCode::Home, KeyModifiers::NONE);
    press(&mut url, KeyCode::Right, KeyModifiers::NONE);
    press(&mut url, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(url.as_str(), "ttps://wxample.com");
    assert_eq!(url.cursor(), 0);
    press(&mut url, KeyCode::End, KeyModifiers::NONE);
    assert_eq!(url.cursor(), url.as_str().len());
}

#[test]
fn word_keys_and_kills_act_around_the_cursor() {
    let mut text = line("one two three", "one two".len());
    press(&mut text, KeyCode::Backspace, KeyModifiers::CONTROL);
    assert_eq!((text.as_str(), text.cursor()), ("one  three", 4));
    press(&mut text, KeyCode::Char('d'), KeyModifiers::ALT);
    assert_eq!((text.as_str(), text.cursor()), ("one ", 4));
    let mut text = line("one two three", 4);
    press(&mut text, KeyCode::Right, KeyModifiers::CONTROL);
    assert_eq!(text.cursor(), "one two".len());
    press(&mut text, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!((text.as_str(), text.cursor()), (" three", 0));
    press(&mut text, KeyCode::Char('e'), KeyModifiers::CONTROL);
    press(&mut text, KeyCode::Char('b'), KeyModifiers::ALT);
    press(&mut text, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(text.as_str(), " ");
}

#[test]
fn row_keys_pass_through_while_the_line_cannot_use_them() {
    let mut empty = LineEdit::default();
    for code in [KeyCode::Left, KeyCode::Right, KeyCode::Delete] {
        assert_eq!(press(&mut empty, code, KeyModifiers::NONE), None);
    }
    let mut typed = line("ab", 2);
    assert_eq!(press(&mut typed, KeyCode::Delete, KeyModifiers::NONE), None);
    assert_eq!(
        press(&mut typed, KeyCode::Right, KeyModifiers::NONE),
        Some(LineKey::Moved)
    );
    assert_eq!(press(&mut typed, KeyCode::Up, KeyModifiers::NONE), None);
}

#[test]
fn the_cursor_stays_between_graphemes_that_an_edit_joins() {
    let mut family = line("👩👧", "👩".len());
    family.insert("\u{200d}");
    assert_eq!(family.cursor(), family.as_str().len());
    press(&mut family, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(family.as_str(), "");

    let mut flag = line("\u{1f1fa}x\u{1f1f8}", "\u{1f1fa}x".len());
    press(&mut flag, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!((flag.as_str(), flag.cursor()), ("🇺🇸", 0));

    let mut accent = line("a \u{301}b", "a \u{301}b".len());
    press(&mut accent, KeyCode::Left, KeyModifiers::CONTROL);
    assert_eq!(accent.cursor(), 1);
    press(&mut accent, KeyCode::Right, KeyModifiers::CONTROL);
    assert_eq!(accent.cursor(), accent.as_str().len());
    press(&mut accent, KeyCode::Backspace, KeyModifiers::CONTROL);
    assert_eq!((accent.as_str(), accent.cursor()), ("a", 1));
}

#[test]
fn insert_drops_control_characters() {
    let mut text = line("ac", 1);
    text.insert("b\n\t");
    assert_eq!((text.as_str(), text.cursor()), ("abc", 2));
}
