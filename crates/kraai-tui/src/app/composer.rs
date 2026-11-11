use super::*;

impl App {
    pub(super) fn handle_composer_shortcut(&mut self, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('v') if key.modifiers == KeyModifiers::CONTROL => self.paste_image(),
            KeyCode::Left if control => {
                self.state.input_cursor = self
                    .state
                    .draft_images
                    .snap(word_left(&self.state.input, self.state.input_cursor), false)
            }
            KeyCode::Right if control => {
                self.state.input_cursor = self
                    .state
                    .draft_images
                    .snap(word_right(&self.state.input, self.state.input_cursor), true)
            }
            KeyCode::Char('b') if alt => {
                self.state.input_cursor = self
                    .state
                    .draft_images
                    .snap(word_left(&self.state.input, self.state.input_cursor), false)
            }
            KeyCode::Char('f') if alt => {
                self.state.input_cursor = self
                    .state
                    .draft_images
                    .snap(word_right(&self.state.input, self.state.input_cursor), true)
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
        let cursor = self.state.input_cursor;
        let boundary = if forward {
            word_right(&self.state.input, cursor)
        } else {
            word_left(&self.state.input, cursor)
        };
        self.replace_input_range(cursor.min(boundary)..cursor.max(boundary), "");
    }
}

pub(super) fn word_left(input: &str, cursor: usize) -> usize {
    let mut position = cursor;
    let mut word = false;
    for (index, ch) in input.get(..cursor).unwrap_or_default().char_indices().rev() {
        if word && ch.is_whitespace() {
            break;
        }
        word |= !ch.is_whitespace();
        position = index;
    }
    position
}

fn word_right(input: &str, cursor: usize) -> usize {
    let mut word = false;
    for (index, ch) in input.get(cursor..).unwrap_or_default().char_indices() {
        if word && ch.is_whitespace() {
            return cursor + index;
        }
        word |= !ch.is_whitespace();
    }
    input.len()
}
