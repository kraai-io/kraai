use super::{App, KeyCode, KeyEvent, KeyModifiers};

impl App {
    pub(super) fn handle_error_key(&mut self, key: KeyEvent) -> bool {
        if self.state.last_error.is_none() {
            return false;
        }
        if !self.state.error_open {
            if key.code == KeyCode::F(8) {
                self.state.error_open = true;
                self.state.error_scroll.set(0);
                return true;
            }
            return false;
        }
        let scroll = self.state.error_scroll.get();
        match key.code {
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.error_open = false;
            }
            KeyCode::Esc | KeyCode::F(8) => self.state.error_open = false,
            KeyCode::Up => self.state.error_scroll.set(scroll.saturating_sub(1)),
            KeyCode::Down => self.state.error_scroll.set(scroll.saturating_add(1)),
            KeyCode::PageUp => self.state.error_scroll.set(scroll.saturating_sub(10)),
            KeyCode::PageDown => self.state.error_scroll.set(scroll.saturating_add(10)),
            KeyCode::Home => self.state.error_scroll.set(0),
            KeyCode::End => self.state.error_scroll.set(usize::MAX),
            KeyCode::Char('c') if key.modifiers.is_empty() => {
                if let Some(error) = self.state.last_error.clone()
                    && let Err(error) =
                        self.copy_text_to_clipboard(&error, super::feedback::CopyTarget::Error)
                {
                    self.state.status = format!("Copy failed: {error}");
                }
            }
            KeyCode::Char('d') if key.modifiers.is_empty() => {
                if self.state.last_error.as_ref() == Some(&self.state.status) {
                    self.state.status.clear();
                }
                self.state.last_error = None;
                self.state.error_open = false;
                self.state.error_scroll.set(0);
            }
            _ => {}
        }
        true
    }
}
