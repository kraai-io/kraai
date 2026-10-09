use super::CrosstermEvent;
use super::terminal_reply::{FilteredEvent, TerminalReply};
use crate::terminal_cursor::CursorStyle;

#[derive(Default)]
pub(super) struct CursorPreferences {
    reply: TerminalReply,
    pub(super) style: Option<CursorStyle>,
}

impl CursorPreferences {
    pub(super) fn request(&mut self) {
        #[cfg(unix)]
        self.reply
            .request(b"\x1bP$q q\x1b\\", &mut std::io::stdout().lock());
    }

    pub(super) fn custom_blink(&self) -> bool {
        CursorStyle::custom_blink(self.style)
    }

    pub(super) fn filter(&mut self, event: CrosstermEvent) -> FilteredEvent {
        self.reply.filter(
            event,
            'P',
            |response| {
                ["0$r", "1$r"].iter().any(|prefix| {
                    prefix.starts_with(response)
                        || response.strip_prefix(prefix).is_some_and(|suffix| {
                            suffix.len() <= 8
                                && suffix
                                    .chars()
                                    .all(|ch| ch.is_ascii_digit() || ch == ' ' || ch == 'q')
                        })
                })
            },
            |response| {
                if response.starts_with("0$r") {
                    return true;
                }
                let Some(code) = response
                    .strip_prefix("1$r")
                    .and_then(|value| value.strip_suffix(" q"))
                else {
                    return false;
                };
                self.style = code.parse().ok().and_then(CursorStyle::from_report);
                true
            },
        )
    }
}

#[cfg(test)]
mod tests {
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
        match preferences.filter(key.clone()) {
            FilteredEvent::Replay(events) => assert_eq!(events, vec![start(), key]),
            _ => unreachable!(),
        }
        preferences.filter(start());
        for ch in "1$r5 q".chars() {
            preferences.filter(character(ch));
        }
        preferences.filter(finish());
        assert_eq!(preferences.style, CursorStyle::from_report(5));
    }
}
