use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

/// One line of text with a cursor, edited the way a shell prompt is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LineEdit {
    text: String,
    /// Byte offset that always sits on a grapheme boundary.
    cursor: usize,
}

/// What a key did to a [`LineEdit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LineKey {
    /// The text changed.
    Edited,
    /// Only the cursor moved.
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

    /// Replaces the text and puts the cursor after it.
    pub(super) fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Inserts `text` at the cursor, dropping control characters.
    pub(super) fn insert(&mut self, text: &str) {
        let text: String = text.chars().filter(|ch| !ch.is_control()).collect();
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    /// Applies an editing or cursor key. Left, Right and Delete act only while there is text for
    /// them to act on, so a sheet can give them a meaning of its own on an empty line or, for
    /// Delete, at its end.
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Option<LineKey> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let text = !self.text.is_empty();
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

    /// Removes the text between the cursor and `boundary`.
    fn remove_to(&mut self, boundary: usize) -> Option<LineKey> {
        let range = self.cursor.min(boundary)..self.cursor.max(boundary);
        if range.is_empty() {
            return Some(LineKey::Moved);
        }
        self.cursor = range.start;
        self.text.replace_range(range, "");
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
        word_left(&self.text, self.cursor)
    }

    fn word_right(&self) -> usize {
        word_right(&self.text, self.cursor)
    }
}

/// The start of the word before `cursor`, skipping whitespace first.
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

/// The end of the word after `cursor`, skipping whitespace first.
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
mod tests {
    use super::*;

    fn line(text: &str, cursor: usize) -> LineEdit {
        LineEdit {
            text: text.to_owned(),
            cursor,
        }
    }

    fn press(line: &mut LineEdit, code: KeyCode, modifiers: KeyModifiers) -> Option<LineKey> {
        line.handle_key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn arrows_move_through_graphemes_and_typing_inserts_at_the_cursor() {
        let mut url = LineEdit::default();
        url.set("https://e\u{301}xample.com");
        for _ in 0.."xample.com".len() {
            press(&mut url, KeyCode::Left, KeyModifiers::NONE);
        }
        assert_eq!(url.cursor(), "https://e\u{301}".len());
        assert_eq!(
            press(&mut url, KeyCode::Left, KeyModifiers::NONE),
            Some(LineKey::Moved)
        );
        assert_eq!(url.cursor(), "https://".len());
        press(&mut url, KeyCode::Char('w'), KeyModifiers::NONE);
        assert_eq!(url.as_str(), "https://we\u{301}xample.com");
        press(&mut url, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(url.as_str(), "https://wxample.com");
        press(&mut url, KeyCode::Home, KeyModifiers::NONE);
        press(&mut url, KeyCode::Right, KeyModifiers::NONE);
        press(&mut url, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(url.as_str(), "ttps://wxample.com");
        assert_eq!(url.cursor(), 0);
        press(&mut url, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(url.cursor(), url.as_str().len());
    }

    #[test]
    fn word_keys_and_kills_act_around_the_cursor() {
        let mut text = line("one two three", "one two".len());
        press(&mut text, KeyCode::Backspace, KeyModifiers::CONTROL);
        assert_eq!((text.as_str(), text.cursor()), ("one  three", 4));
        press(&mut text, KeyCode::Char('d'), KeyModifiers::ALT);
        assert_eq!((text.as_str(), text.cursor()), ("one ", 4));
        let mut text = line("one two three", 4);
        press(&mut text, KeyCode::Right, KeyModifiers::CONTROL);
        assert_eq!(text.cursor(), "one two".len());
        press(&mut text, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!((text.as_str(), text.cursor()), (" three", 0));
        press(&mut text, KeyCode::Char('e'), KeyModifiers::CONTROL);
        press(&mut text, KeyCode::Char('b'), KeyModifiers::ALT);
        press(&mut text, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(text.as_str(), " ");
    }

    #[test]
    fn row_keys_pass_through_while_the_line_cannot_use_them() {
        let mut empty = LineEdit::default();
        for code in [KeyCode::Left, KeyCode::Right, KeyCode::Delete] {
            assert_eq!(press(&mut empty, code, KeyModifiers::NONE), None);
        }
        let mut typed = line("ab", 2);
        assert_eq!(press(&mut typed, KeyCode::Delete, KeyModifiers::NONE), None);
        assert_eq!(
            press(&mut typed, KeyCode::Right, KeyModifiers::NONE),
            Some(LineKey::Moved)
        );
        assert_eq!(press(&mut typed, KeyCode::Up, KeyModifiers::NONE), None);
    }

    #[test]
    fn insert_drops_control_characters() {
        let mut text = line("ac", 1);
        text.insert("b\n\t");
        assert_eq!((text.as_str(), text.cursor()), ("abc", 2));
    }
}
