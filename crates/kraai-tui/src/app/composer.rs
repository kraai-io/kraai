use super::App;
use super::line_edit::{word_left, word_right};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl App {
    pub(super) fn handle_composer_shortcut(&mut self, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "only the listed keys are composer shortcuts"
        )]
        match key.code {
            KeyCode::Char('v') if key.modifiers == KeyModifiers::CONTROL => self.paste_image(),
            KeyCode::Left if control => {
                self.state.composer.cursor = self.state.composer.images.snap(
                    word_left(&self.state.composer.text, self.state.composer.cursor),
                    false,
                )
            }
            KeyCode::Right if control => {
                self.state.composer.cursor = self.state.composer.images.snap(
                    word_right(&self.state.composer.text, self.state.composer.cursor),
                    true,
                )
            }
            KeyCode::Char('b') if alt => {
                self.state.composer.cursor = self.state.composer.images.snap(
                    word_left(&self.state.composer.text, self.state.composer.cursor),
                    false,
                )
            }
            KeyCode::Char('f') if alt => {
                self.state.composer.cursor = self.state.composer.images.snap(
                    word_right(&self.state.composer.text, self.state.composer.cursor),
                    true,
                )
            }
            KeyCode::Backspace if control || alt => self.delete_input_word(false),
            KeyCode::Delete if control => self.delete_input_word(true),
            KeyCode::Char('d') if alt => self.delete_input_word(true),
            _ => return false,
        }
        self.reset_command_selection();
        true
    }

    fn delete_input_word(&mut self, forward: bool) {
        self.reset_input_history_navigation();
        let cursor = self.state.composer.cursor;
        let boundary = if forward {
            word_right(&self.state.composer.text, cursor)
        } else {
            word_left(&self.state.composer.text, cursor)
        };
        self.replace_input_range(cursor.min(boundary)..cursor.max(boundary), "");
    }
}
