use super::*;
use crate::app::{KeyCode, KeyEvent, KeyModifiers};

fn start() -> CrosstermEvent {
    CrosstermEvent::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT))
}

fn character(ch: char) -> CrosstermEvent {
    CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Char(ch),
        if ch.is_ascii_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        },
    ))
}

#[test]
fn request_writes_the_query_and_input_after_the_reply_passes_through() {
    let mut palette = TerminalPalette::default();
    let mut query = Vec::new();
    palette.request_with(&mut query);
    assert_eq!(query, b"\x1b]4;8;?\x07");
    palette.filter(start());
    for ch in "4;8;rgb:5858/5252/7373".chars() {
        palette.filter(character(ch));
    }
    assert!(matches!(
        palette.filter(CrosstermEvent::Key(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        ))),
        PaletteEvent::Consumed
    ));
    assert_eq!(palette.muted, Some(Color::Rgb(88, 82, 115)));
    assert!(matches!(
        palette.filter(character('x')),
        PaletteEvent::Pass(_)
    ));
}

#[test]
fn unrelated_input_does_not_disable_reply_capture() {
    let mut palette = TerminalPalette {
        reply: TerminalReply::pending(),
        ..Default::default()
    };
    assert!(matches!(
        palette.filter(character('x')),
        PaletteEvent::Pass(_)
    ));
    palette.filter(start());
    let key = character('x');
    let PaletteEvent::Replay(events) = palette.filter(key.clone()) else {
        unreachable!();
    };
    assert_eq!(events, vec![start(), key]);
    assert!(matches!(palette.filter(start()), PaletteEvent::Consumed));
    for ch in "4;8;rgb:AAAA/BBBB/CCCC".chars() {
        assert!(matches!(
            palette.filter(character(ch)),
            PaletteEvent::Consumed
        ));
    }
    assert!(matches!(
        palette.filter(CrosstermEvent::Key(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        ))),
        PaletteEvent::Consumed
    ));
    assert_eq!(palette.muted, Some(Color::Rgb(170, 187, 204)));
}

#[test]
fn unterminated_candidates_have_bounded_buffering() {
    let mut palette = TerminalPalette {
        reply: TerminalReply::pending(),
        ..Default::default()
    };
    let events: Vec<_> = std::iter::once(start())
        .chain("4;8;rgb:".chars().map(character))
        .chain(std::iter::repeat_n(character('a'), 100))
        .collect();
    let mut replayed = Vec::new();
    for event in &events {
        match palette.filter(event.clone()) {
            PaletteEvent::Pass(event) => replayed.push(event),
            PaletteEvent::Replay(events) => replayed.extend(events),
            PaletteEvent::Consumed => {}
        }
        assert!(palette.reply.held.len() <= 23);
    }
    assert_eq!(replayed, events);
    assert!(palette.reply.held.is_empty());
}

#[test]
fn palette_reply_accepts_both_terminators_and_preserves_other_input() {
    for terminator in [
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT),
    ] {
        let mut palette = TerminalPalette {
            reply: TerminalReply::pending(),
            ..Default::default()
        };
        let start = CrosstermEvent::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT));
        assert!(matches!(
            palette.filter(start.clone()),
            PaletteEvent::Consumed
        ));
        assert!(matches!(
            palette.filter(CrosstermEvent::Resize(80, 24)),
            PaletteEvent::Pass(_)
        ));
        for ch in "4;8;rgb:5858/5252/7373".chars() {
            assert!(matches!(
                palette.filter(CrosstermEvent::Key(KeyEvent::new(
                    KeyCode::Char(ch),
                    KeyModifiers::NONE
                ))),
                PaletteEvent::Consumed
            ));
        }
        assert!(matches!(
            palette.filter(CrosstermEvent::Key(terminator)),
            PaletteEvent::Consumed
        ));
        assert_eq!(palette.muted, Some(Color::Rgb(88, 82, 115)));
        let mut palette = TerminalPalette {
            reply: TerminalReply::pending(),
            ..Default::default()
        };
        palette.filter(start.clone());
        let key = CrosstermEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let PaletteEvent::Replay(events) = palette.filter(key.clone()) else {
            unreachable!();
        };
        assert_eq!(events, vec![start, key]);
    }
    assert_eq!(
        parse_color("4;8;rgb:58/52/73"),
        Some(Color::Rgb(88, 82, 115))
    );
    assert_eq!(parse_color("4;8;rgb:gg/52/73"), None);
}
