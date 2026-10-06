use super::CrosstermEvent;
pub(super) use super::terminal_reply::FilteredEvent as PaletteEvent;
use super::terminal_reply::TerminalReply;
use ratatui::style::Color;

#[derive(Default)]
pub(super) struct TerminalPalette {
    pub(super) muted: Option<Color>,
    reply: TerminalReply,
}

impl TerminalPalette {
    pub(super) fn request(&mut self) {
        #[cfg(unix)]
        self.request_with(&mut std::io::stdout().lock());
    }

    #[cfg(any(unix, test))]
    fn request_with(&mut self, output: &mut impl std::io::Write) {
        self.reply.request(b"\x1b]4;8;?\x07", output);
    }

    pub(super) fn filter(&mut self, event: CrosstermEvent) -> PaletteEvent {
        self.reply.filter(
            event,
            ']',
            |response| {
                let prefix = "4;8;rgb:";
                response.len() <= prefix.len() && prefix.starts_with(response)
                    || response.starts_with(prefix)
                        && response.len() <= 22
                        && response.get(prefix.len()..).is_some_and(|value| {
                            value.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '/')
                        })
            },
            |response| {
                if let Some(color) = parse_color(response) {
                    self.muted = Some(color);
                    true
                } else {
                    false
                }
            },
        )
    }
}

fn parse_color(response: &str) -> Option<Color> {
    let mut parts = response.strip_prefix("4;8;rgb:")?.split('/');
    let mut channel = || {
        let value = parts.next()?;
        if value.is_empty() || value.len() > 4 {
            return None;
        }
        let number = u32::from_str_radix(value, 16).ok()?;
        let maximum = (1u32 << (value.len() * 4)) - 1;
        Some(((number * 255 + maximum / 2) / maximum) as u8)
    };
    let color = Color::Rgb(channel()?, channel()?, channel()?);
    if parts.next().is_some() {
        return None;
    }
    Some(color)
}

#[cfg(test)]
mod tests {
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
    fn delayed_and_partial_replies_never_become_input() {
        let mut palette = TerminalPalette::default();
        let mut query = Vec::new();
        palette.request_with(&mut query);
        assert_eq!(query, b"\x1b]4;8;?\x07");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(matches!(palette.filter(start()), PaletteEvent::Consumed));
        for ch in "4;8;rgb:5858/".chars() {
            assert!(matches!(
                palette.filter(character(ch)),
                PaletteEvent::Consumed
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(1100));
        for ch in "5252/7373".chars() {
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
        match palette.filter(key.clone()) {
            PaletteEvent::Replay(events) => assert_eq!(events, vec![start(), key]),
            _ => unreachable!(),
        }
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
            match palette.filter(key.clone()) {
                PaletteEvent::Replay(events) => assert_eq!(events, vec![start, key]),
                _ => unreachable!(),
            }
        }
        assert_eq!(
            parse_color("4;8;rgb:58/52/73"),
            Some(Color::Rgb(88, 82, 115))
        );
        assert_eq!(parse_color("4;8;rgb:gg/52/73"), None);
    }
}
