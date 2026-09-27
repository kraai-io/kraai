use super::{CrosstermEvent, KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;

#[derive(Default)]
pub(super) struct TerminalPalette {
    pub(super) muted: Option<Color>,
    pending: bool,
    response: String,
    held: Vec<CrosstermEvent>,
}

pub(super) enum PaletteEvent {
    Pass(CrosstermEvent),
    Consumed,
    Replay(Vec<CrosstermEvent>),
}

impl TerminalPalette {
    pub(super) fn request(&mut self) {
        #[cfg(unix)]
        {
            use std::io::Write;
            self.pending = std::io::stdout()
                .write_all(b"\x1b]4;8;?\x07")
                .and_then(|()| std::io::stdout().flush())
                .is_ok();
        }
    }

    pub(super) fn filter(&mut self, event: CrosstermEvent) -> PaletteEvent {
        if !self.pending {
            return PaletteEvent::Pass(event);
        }
        let CrosstermEvent::Key(key) = &event else {
            return PaletteEvent::Pass(event);
        };
        if self.held.is_empty() {
            if key.code == KeyCode::Char(']') && key.modifiers == KeyModifiers::ALT {
                self.held.push(event);
                return PaletteEvent::Consumed;
            }
            return PaletteEvent::Pass(event);
        }
        let terminal = matches!(
            key,
            KeyEvent {
                code: KeyCode::Char('g'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
        ) || matches!(
            key,
            KeyEvent {
                code: KeyCode::Char('\\'),
                modifiers: KeyModifiers::ALT,
                ..
            }
        );
        if terminal {
            self.held.push(event);
            if let Some(color) = parse_color(&self.response) {
                self.muted = Some(color);
                self.pending = false;
                self.held.clear();
                self.response.clear();
                return PaletteEvent::Consumed;
            }
            return self.replay();
        }
        if let KeyCode::Char(ch) = key.code
            && key.modifiers.is_empty()
        {
            self.response.push(ch);
            self.held.push(event);
            let prefix = "4;8;rgb:";
            if self.response.len() <= prefix.len() && prefix.starts_with(&self.response)
                || self.response.starts_with(prefix)
                    && self.response.len() <= 22
                    && self.response.get(prefix.len()..).is_some_and(|value| {
                        value.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '/')
                    })
            {
                return PaletteEvent::Consumed;
            }
        } else {
            self.held.push(event);
        }
        self.replay()
    }

    fn replay(&mut self) -> PaletteEvent {
        self.response.clear();
        PaletteEvent::Replay(std::mem::take(&mut self.held))
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

    #[test]
    fn palette_reply_accepts_both_terminators_and_preserves_other_input() {
        for terminator in [
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT),
        ] {
            let mut palette = TerminalPalette {
                pending: true,
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
                pending: true,
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
