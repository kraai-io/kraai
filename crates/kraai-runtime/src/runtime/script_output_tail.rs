use std::collections::VecDeque;

use kraai_execution_types::ScriptOutputStream;

use crate::ScriptOutputLine;

const LINES: usize = 8;
const LINE_BYTES: usize = 512;

#[derive(Default)]
pub(crate) struct OutputTail {
    lines: VecDeque<ScriptOutputLine>,
    stdout: OpenLine,
    stderr: OpenLine,
}

#[derive(Default)]
struct OpenLine {
    bytes: Vec<u8>,
    cut: bool,
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
mod tests;
