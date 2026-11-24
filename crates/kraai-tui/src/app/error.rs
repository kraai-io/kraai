use super::App;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl App {
    pub(super) fn open_error(&mut self) {
        if self.state.errors.last.is_some() {
            self.state.errors.open = true;
            self.state.errors.notice = None;
            self.state.errors.scroll.set(0);
        }
    }

    pub(super) fn handle_error_key(&mut self, key: KeyEvent) -> bool {
        if self.state.errors.last.is_none() || !self.state.errors.open {
            return false;
        }
        let scroll = self.state.errors.scroll.get();
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "the error view swallows every other key"
        )]
        match key.code {
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.errors.open = false;
            }
            KeyCode::Esc => self.state.errors.open = false,
            KeyCode::Up => self.state.errors.scroll.set(scroll.saturating_sub(1)),
            KeyCode::Down => self.state.errors.scroll.set(scroll.saturating_add(1)),
            KeyCode::PageUp => self.state.errors.scroll.set(scroll.saturating_sub(10)),
            KeyCode::PageDown => self.state.errors.scroll.set(scroll.saturating_add(10)),
            KeyCode::Home => self.state.errors.scroll.set(0),
            KeyCode::End => self.state.errors.scroll.set(usize::MAX),
            KeyCode::Char('c') if key.modifiers.is_empty() => {
                if let Some(error) = self.state.errors.last.clone() {
                    self.state.errors.notice = self
                        .copy_text_to_clipboard(&error, super::feedback::CopyTarget::Error)
                        .err()
                        .map(|error| format!("Copy failed: {error}"));
                }
            }
            KeyCode::Char('d') if key.modifiers.is_empty() => self.clear_error(),
            _ => {}
        }
        true
    }
}
