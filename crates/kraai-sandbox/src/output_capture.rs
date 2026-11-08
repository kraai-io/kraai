use std::collections::VecDeque;

pub const CAPTURE_HEAD_BYTES: usize = 32 * 1024;
pub const CAPTURE_TAIL_BYTES: usize = 32 * 1024;
pub const MAX_CAPTURE_BYTES: usize = CAPTURE_HEAD_BYTES + CAPTURE_TAIL_BYTES;

/// Keeps the first and last bytes of a stream. The head is emitted as it arrives; the tail,
/// preceded by a notice when the middle was dropped, is emitted by [`OutputCapture::finish`].
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
mod tests {
    use super::*;

    fn capture(chunks: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
        let mut capture = OutputCapture::new();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend_from_slice(capture.push(&chunk));
        }
        events.extend_from_slice(capture.finish());
        let captured = capture.into_bytes();
        assert_eq!(events, captured);
        captured
    }

    fn notice(omitted: usize) -> Vec<u8> {
        format!("\n[kraai: {omitted} bytes omitted from the middle of this output; redirect large output to a file and inspect selected excerpts]\n").into_bytes()
    }

    #[test]
    fn output_up_to_the_limit_is_kept_whole() {
        let full = vec![b'x'; MAX_CAPTURE_BYTES];
        assert_eq!(capture([full.clone()]), full);
        assert_eq!(
            capture(full.chunks(1000).map(<[u8]>::to_vec)),
            full,
            "chunked output past the head stays whole"
        );
    }

    #[test]
    fn long_output_keeps_the_head_and_tail_and_counts_the_middle() {
        let head = vec![b'h'; CAPTURE_HEAD_BYTES];
        let middle = vec![b'm'; MAX_CAPTURE_BYTES * 3 + 7];
        let tail = vec![b't'; CAPTURE_TAIL_BYTES];
        let mut expected = head.clone();
        expected.extend(notice(middle.len()));
        expected.extend(&tail);
        assert_eq!(
            capture([head.clone(), middle.clone(), tail.clone()]),
            expected
        );
        let mut all = head;
        all.extend(middle);
        all.extend(tail);
        assert_eq!(capture(all.chunks(4096).map(<[u8]>::to_vec)), expected);
    }

    #[test]
    fn truncation_keeps_unicode_valid_at_each_byte_boundary() {
        for shift in 0..4 {
            let mut output = vec![b'x'; CAPTURE_HEAD_BYTES - shift];
            output.extend("🦀".repeat(MAX_CAPTURE_BYTES).as_bytes());
            output.extend("z".repeat(shift + 1).as_bytes());
            for chunked in [false, true] {
                let chunks = if chunked {
                    output.chunks(1).map(<[u8]>::to_vec).collect()
                } else {
                    vec![output.clone()]
                };
                let captured = capture(chunks);
                assert!(std::str::from_utf8(&captured).is_ok());
                let text = String::from_utf8_lossy(&captured);
                assert!(text.ends_with(&format!("🦀{}", "z".repeat(shift + 1))));
                assert!(text.contains("bytes omitted from the middle"));
                assert!(text.len() <= MAX_CAPTURE_BYTES + notice(output.len()).len());
            }
        }
    }

    #[test]
    fn untruncated_binary_and_split_unicode_are_preserved() {
        for bytes in ["a🦀z".as_bytes(), &[0xff, 0x00, 0xe2, 0x82]] {
            assert_eq!(capture(bytes.iter().map(|byte| vec![*byte])), bytes);
        }
    }
}
