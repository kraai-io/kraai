use super::terminal_reply::{FilteredEvent, TerminalReply};
use crate::terminal_cursor::CursorStyle;
use ratatui::crossterm::event::Event as CrosstermEvent;

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
mod tests;
