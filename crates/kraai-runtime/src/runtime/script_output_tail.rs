use std::collections::VecDeque;

use kraai_execution_types::ScriptOutputStream;

use crate::ScriptOutputLine;

/// How many finished lines the tail keeps.
const LINES: usize = 8;
/// How much of one line the tail keeps. The rest of a longer line is dropped.
const LINE_BYTES: usize = 512;

/// The last lines a running script printed, across both of its streams, in the order they
/// ended. A carriage return starts its line over, the way a terminal draws a progress bar.
#[derive(Default)]
pub(crate) struct OutputTail {
    lines: VecDeque<ScriptOutputLine>,
    stdout: OpenLine,
    stderr: OpenLine,
}

#[derive(Default)]
struct OpenLine {
    bytes: Vec<u8>,
    /// The line held more than `LINE_BYTES`.
    cut: bool,
    /// The last byte was a carriage return that a line feed has not yet followed.
    carriage_return: bool,
}

impl OpenLine {
    fn push(&mut self, byte: u8) {
        if std::mem::take(&mut self.carriage_return) {
            self.bytes.clear();
            self.cut = false;
        }
        if self.bytes.len() < LINE_BYTES {
            self.bytes.push(byte);
        } else {
            self.cut = true;
        }
    }

    fn text(&self) -> String {
        let text = String::from_utf8_lossy(&self.bytes);
        let text = if self.cut {
            text.trim_end_matches('\u{FFFD}')
        } else {
            &text
        };
        text.trim_end().to_owned()
    }

    fn take(&mut self) -> String {
        let text = self.text();
        *self = Self::default();
        text
    }
}

impl OutputTail {
    pub(crate) fn push(&mut self, stream: ScriptOutputStream, bytes: &[u8]) {
        for &byte in bytes {
            let open = self.open(stream);
            match byte {
                b'\n' => {
                    let text = open.take();
                    if !text.is_empty() {
                        if self.lines.len() == LINES {
                            self.lines.pop_front();
                        }
                        self.lines.push_back(ScriptOutputLine { stream, text });
                    }
                }
                b'\r' => open.carriage_return = true,
                byte => open.push(byte),
            }
        }
    }

    /// The finished lines, then whatever each stream has printed of a line it has not ended.
    pub(crate) fn lines(&self) -> Vec<ScriptOutputLine> {
        let open = [
            (ScriptOutputStream::Stdout, &self.stdout),
            (ScriptOutputStream::Stderr, &self.stderr),
        ]
        .into_iter()
        .filter_map(|(stream, open)| {
            let text = open.text();
            (!text.is_empty()).then_some(ScriptOutputLine { stream, text })
        });
        let mut lines: Vec<_> = self.lines.iter().cloned().chain(open).collect();
        let excess = lines.len().saturating_sub(LINES);
        lines.drain(..excess);
        lines
    }

    fn open(&mut self, stream: ScriptOutputStream) -> &mut OpenLine {
        match stream {
            ScriptOutputStream::Stdout => &mut self.stdout,
            ScriptOutputStream::Stderr => &mut self.stderr,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(tail: &OutputTail) -> Vec<(ScriptOutputStream, String)> {
        tail.lines()
            .into_iter()
            .map(|line| (line.stream, line.text))
            .collect()
    }

    use ScriptOutputStream::{Stderr, Stdout};

    #[test]
    fn keeps_the_last_lines_of_both_streams_in_the_order_they_ended() {
        let mut tail = OutputTail::default();
        tail.push(Stdout, b"one\ntw");
        tail.push(Stderr, b"warn\n");
        tail.push(Stdout, b"o\nthree");
        assert_eq!(
            texts(&tail),
            [
                (Stdout, String::from("one")),
                (Stderr, String::from("warn")),
                (Stdout, String::from("two")),
                (Stdout, String::from("three")),
            ]
        );

        for index in 0..20 {
            tail.push(Stdout, format!("line {index}\n").as_bytes());
        }
        let lines = texts(&tail);
        assert_eq!(lines.len(), LINES);
        assert_eq!(lines.last(), Some(&(Stdout, String::from("line 19"))));
        assert_eq!(lines.first(), Some(&(Stdout, String::from("line 12"))));
    }

    #[test]
    fn blank_lines_are_skipped_and_line_endings_are_trimmed() {
        let mut tail = OutputTail::default();
        tail.push(Stdout, b"\n\n  \nfirst  \r\nsecond\r\n\n");
        assert_eq!(
            texts(&tail),
            [
                (Stdout, String::from("first")),
                (Stdout, String::from("second"))
            ]
        );
    }

    #[test]
    fn a_carriage_return_starts_the_line_over() {
        let mut tail = OutputTail::default();
        tail.push(Stderr, b"Building 1/3\rBuilding 2/3\r");
        assert_eq!(texts(&tail), [(Stderr, String::from("Building 2/3"))]);
        tail.push(Stderr, b"Building 3/3\ndone\n");
        assert_eq!(
            texts(&tail),
            [
                (Stderr, String::from("Building 3/3")),
                (Stderr, String::from("done")),
            ]
        );
    }

    #[test]
    fn long_lines_keep_their_start_and_never_end_in_half_a_character() {
        let mut tail = OutputTail::default();
        let mut long = vec![b'a'; LINE_BYTES - 1];
        long.extend("é and more".as_bytes());
        long.push(b'\n');
        tail.push(Stdout, &long);
        let lines = texts(&tail);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines.first().map(|(_, text)| text.clone()),
            Some("a".repeat(LINE_BYTES - 1))
        );
    }

    #[test]
    fn a_character_split_across_chunks_is_joined() {
        let mut tail = OutputTail::default();
        let (head, rest) = "héllo\n".as_bytes().split_at(2);
        tail.push(Stdout, head);
        tail.push(Stdout, rest);
        assert_eq!(texts(&tail), [(Stdout, String::from("héllo"))]);
    }
}
