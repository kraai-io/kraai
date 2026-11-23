use std::collections::VecDeque;

pub const CAPTURE_HEAD_BYTES: usize = 32 * 1024;
pub const CAPTURE_TAIL_BYTES: usize = 32 * 1024;
pub const MAX_CAPTURE_BYTES: usize = CAPTURE_HEAD_BYTES + CAPTURE_TAIL_BYTES;

#[derive(Default)]
pub struct OutputCapture {
    bytes: Vec<u8>,
    emitted: usize,
    head_full: bool,
    tail: VecDeque<u8>,
    omitted: usize,
    finished: bool,
}

impl OutputCapture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) -> &[u8] {
        if self.finished {
            return &[];
        }
        if self.head_full {
            self.push_tail(bytes);
            return &[];
        }
        let room = CAPTURE_HEAD_BYTES.saturating_sub(self.bytes.len());
        let (head, rest) = bytes.split_at_checked(room).unwrap_or((bytes, &[]));
        self.bytes.extend_from_slice(head);
        if !rest.is_empty() {
            let split = self.bytes.split_off(complete_utf8_prefix(&self.bytes));
            self.head_full = true;
            self.push_tail(&split);
            self.push_tail(rest);
        }
        let end = complete_utf8_prefix(&self.bytes);
        let start = self.emitted;
        self.emitted = end;
        self.bytes.get(start..end).unwrap_or_default()
    }

    pub fn finish(&mut self) -> &[u8] {
        if !self.finished {
            self.finished = true;
            let mut tail = Vec::from(std::mem::take(&mut self.tail));
            if self.omitted > 0 {
                let partial = tail
                    .iter()
                    .take(3)
                    .take_while(|byte| *byte & 0xc0 == 0x80)
                    .count();
                tail.drain(..partial);
                self.omitted += partial;
                self.bytes.extend_from_slice(
                    format!(
                        "\n[kraai: {} bytes omitted from the middle of this output; redirect large output to a file and inspect selected excerpts]\n",
                        self.omitted
                    )
                    .as_bytes(),
                );
            }
            self.bytes.extend_from_slice(&tail);
        }
        let start = self.emitted;
        self.emitted = self.bytes.len();
        self.bytes.get(start..).unwrap_or_default()
    }

    pub fn into_bytes(mut self) -> Vec<u8> {
        self.finish();
        self.bytes
    }

    fn push_tail(&mut self, bytes: &[u8]) {
        let kept = bytes.len().min(CAPTURE_TAIL_BYTES);
        let incoming = bytes.get(bytes.len() - kept..).unwrap_or_default();
        let excess = (self.tail.len() + kept).saturating_sub(CAPTURE_TAIL_BYTES);
        self.tail.drain(..excess);
        self.tail.extend(incoming);
        self.omitted += excess + (bytes.len() - kept);
    }
}

fn complete_utf8_prefix(bytes: &[u8]) -> usize {
    let start = bytes.len().saturating_sub(4);
    let tail = bytes.get(start..).unwrap_or_default();
    let Some(offset) = tail.iter().rposition(|byte| byte & 0xc0 != 0x80) else {
        return bytes.len();
    };
    let last = tail.get(offset..).unwrap_or_default();
    match std::str::from_utf8(last) {
        Err(error) if error.error_len().is_none() => start + offset,
        _ => bytes.len(),
    }
}

#[cfg(test)]
mod tests;
