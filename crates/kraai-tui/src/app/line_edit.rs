use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LineEdit {
    text: String,
    /// Byte offset that always sits on a grapheme boundary.
    cursor: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LineKey {
    Edited,
    Moved,
}

impl LineEdit {
    pub(super) fn as_str(&self) -> &str {
        &self.text
    }

    pub(super) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(super) fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub(super) fn insert(&mut self, text: &str) {
        let text: String = text.chars().filter(|ch| !ch.is_control()).collect();
        self.text.insert_str(self.cursor, &text);
        self.cursor = self.snap(self.cursor + text.len(), true);
    }

    /// Left, Right and Delete act only while there is text for them to act on, so a sheet can
    /// give them a meaning of its own on an empty line or, for Delete, at its end.
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Option<LineKey> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let text = !self.text.is_empty();
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "a key with no line-editing meaning is left to the caller"
        )]
        let target = match key.code {
            KeyCode::Char(ch) if !control && !alt => {
                self.insert(ch.encode_utf8(&mut [0; 4]));
                return Some(LineKey::Edited);
            }
            KeyCode::Backspace if control || alt => return self.remove_to(self.word_left()),
            KeyCode::Backspace => return self.remove_to(self.grapheme_left()),
            KeyCode::Char('u') if control => return self.remove_to(0),
            KeyCode::Char('k') if control => return self.remove_to(self.text.len()),
            KeyCode::Delete if self.cursor < self.text.len() => {
                return self.remove_to(if control {
                    self.word_right()
                } else {
                    self.grapheme_right()
                });
            }
            KeyCode::Char('d') if alt => return self.remove_to(self.word_right()),
            KeyCode::Left if text && control => self.word_left(),
            KeyCode::Right if text && control => self.word_right(),
            KeyCode::Char('b') if alt => self.word_left(),
            KeyCode::Char('f') if alt => self.word_right(),
            KeyCode::Left if text => self.grapheme_left(),
            KeyCode::Right if text => self.grapheme_right(),
            KeyCode::Home => 0,
            KeyCode::Char('a') if control => 0,
            KeyCode::End => self.text.len(),
            KeyCode::Char('e') if control => self.text.len(),
            _ => return None,
        };
        self.cursor = target;
        Some(LineKey::Moved)
    }

    fn remove_to(&mut self, boundary: usize) -> Option<LineKey> {
        let range = self.cursor.min(boundary)..self.cursor.max(boundary);
        if range.is_empty() {
            return Some(LineKey::Moved);
        }
        let start = range.start;
        self.text.replace_range(range, "");
        self.cursor = self.snap(start, false);
        Some(LineKey::Edited)
    }

    fn grapheme_left(&self) -> usize {
        self.text
            .get(..self.cursor)
            .unwrap_or_default()
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn grapheme_right(&self) -> usize {
        self.text
            .get(self.cursor..)
            .unwrap_or_default()
            .graphemes(true)
            .next()
            .map_or(self.cursor, |grapheme| self.cursor + grapheme.len())
    }

    fn word_left(&self) -> usize {
        self.snap(word_left(&self.text, self.cursor), false)
    }

    fn word_right(&self) -> usize {
        self.snap(word_right(&self.text, self.cursor), true)
    }

    /// An edit can join the graphemes on either side of the cursor into one, and a word
    /// boundary can fall before a combining mark.
    fn snap(&self, position: usize, forward: bool) -> usize {
        for (start, grapheme) in self.text.grapheme_indices(true) {
            let end = start + grapheme.len();
            if position < end {
                return if position == start || !forward {
                    start
                } else {
                    end
                };
            }
        }
        self.text.len()
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

pub(super) fn word_right(input: &str, cursor: usize) -> usize {
    let mut word = false;
    for (index, ch) in input.get(cursor..).unwrap_or_default().char_indices() {
        if word && ch.is_whitespace() {
            return cursor + index;
        }
        word |= !ch.is_whitespace();
    }
    input.len()
}

#[cfg(test)]
mod tests;
