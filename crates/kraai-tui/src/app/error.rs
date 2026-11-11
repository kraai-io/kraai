use super::{App, KeyCode, KeyEvent, KeyModifiers};

impl App {
    pub(super) fn open_error(&mut self) {
        if self.state.last_error.is_some() {
            self.state.error_open = true;
            self.state.error_notice = None;
            self.state.error_scroll.set(0);
        }
    }

    pub(super) fn handle_error_key(&mut self, key: KeyEvent) -> bool {
        if self.state.last_error.is_none() || !self.state.error_open {
            return false;
        }
        let scroll = self.state.error_scroll.get();
        match key.code {
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.error_open = false;
            }
            KeyCode::Esc => self.state.error_open = false,
            KeyCode::Up => self.state.error_scroll.set(scroll.saturating_sub(1)),
            KeyCode::Down => self.state.error_scroll.set(scroll.saturating_add(1)),
            KeyCode::PageUp => self.state.error_scroll.set(scroll.saturating_sub(10)),
            KeyCode::PageDown => self.state.error_scroll.set(scroll.saturating_add(10)),
            KeyCode::Home => self.state.error_scroll.set(0),
            KeyCode::End => self.state.error_scroll.set(usize::MAX),
            KeyCode::Char('c') if key.modifiers.is_empty() => {
                if let Some(error) = self.state.last_error.clone() {
                    self.state.error_notice = self
                        .copy_text_to_clipboard(&error, super::feedback::CopyTarget::Error)
                        .err()
                        .map(|error| format!("Copy failed: {error}"));
                }
            }
            KeyCode::Char('d') if key.modifiers.is_empty() => {
                self.state.last_error = None;
                self.state.error_open = false;
                self.state.error_scroll.set(0);
            }
            _ => {}
        }
        true
    }
}
