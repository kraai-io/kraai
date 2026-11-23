use super::*;
use crate::app::{KeyCode, KeyEvent, KeyModifiers};

fn start() -> CrosstermEvent {
    CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Char('P'),
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ))
}

fn character(ch: char) -> CrosstermEvent {
    CrosstermEvent::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
}

fn finish() -> CrosstermEvent {
    CrosstermEvent::Key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT))
}

fn pending() -> CursorPreferences {
    let mut preferences = CursorPreferences::default();
    let mut query = Vec::new();
    preferences.reply.request(b"\x1bP$q q\x1b\\", &mut query);
    assert_eq!(query, b"\x1bP$q q\x1b\\");
    preferences
}

#[test]
fn query_replies_preserve_shape_and_blink_preference_without_becoming_input() {
    for code in 1..=6 {
        let mut preferences = pending();
        assert!(preferences.custom_blink());
        assert!(matches!(
            preferences.filter(character('x')),
            FilteredEvent::Pass(_)
        ));
        assert!(matches!(
            preferences.filter(start()),
            FilteredEvent::Consumed
        ));
        for ch in format!("1$r{code} q").chars() {
            assert!(matches!(
                preferences.filter(character(ch)),
                FilteredEvent::Consumed
            ));
            assert!(matches!(
                preferences.filter(CrosstermEvent::Resize(80, 24)),
                FilteredEvent::Pass(_)
            ));
        }
        assert!(matches!(
            preferences.filter(finish()),
            FilteredEvent::Consumed
        ));
        assert_eq!(preferences.style, CursorStyle::from_report(code));
        assert_eq!(preferences.custom_blink(), matches!(code, 3 | 5));
        assert!(matches!(
            preferences.filter(character('x')),
            FilteredEvent::Pass(_)
        ));
    }
}

#[test]
fn unsupported_and_ambiguous_replies_use_custom_blink_fallback() {
    for response in ["0$r", "0$r q", "1$r0 q", "1$r99 q"] {
        let mut preferences = pending();
        preferences.filter(start());
        for ch in response.chars() {
            assert!(matches!(
                preferences.filter(character(ch)),
                FilteredEvent::Consumed
            ));
        }
        assert!(matches!(
            preferences.filter(finish()),
            FilteredEvent::Consumed
        ));
        assert_eq!(preferences.style, None);
        assert!(preferences.custom_blink());
    }
}

#[test]
fn missing_reply_uses_custom_blink_fallback() {
    assert!(CursorPreferences::default().custom_blink());
    let mut preferences = pending();
    assert!(matches!(
        preferences.filter(character('x')),
        FilteredEvent::Pass(_)
    ));
    assert_eq!(preferences.style, None);
    assert!(preferences.custom_blink());
}

#[test]
fn unrelated_alt_p_input_is_replayed_and_a_later_reply_is_still_accepted() {
    let mut preferences = pending();
    preferences.filter(start());
    let key = character('x');
    let FilteredEvent::Replay(events) = preferences.filter(key.clone()) else {
        unreachable!();
    };
    assert_eq!(events, vec![start(), key]);
    preferences.filter(start());
    for ch in "1$r5 q".chars() {
        preferences.filter(character(ch));
    }
    preferences.filter(finish());
    assert_eq!(preferences.style, CursorStyle::from_report(5));
}
