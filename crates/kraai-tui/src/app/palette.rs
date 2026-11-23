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
mod tests;
