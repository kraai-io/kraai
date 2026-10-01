const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
const TRUNCATION_MARKER: &[u8] = b"\n[kraai: output truncated after 1 MiB; redirect large output to a file and inspect selected excerpts]\n";

pub(super) struct OutputCapture {
    bytes: Vec<u8>,
    emitted: usize,
    truncated: bool,
}

impl OutputCapture {
    pub(super) fn new() -> Self {
        Self {
            bytes: Vec::new(),
            emitted: 0,
            truncated: false,
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) -> &[u8] {
        if self.truncated {
            return &[];
        }
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(bytes.get(..remaining).unwrap_or(bytes));
        if bytes.len() > remaining {
            self.bytes.truncate(complete_utf8_prefix(&self.bytes));
            self.bytes.reserve_exact(TRUNCATION_MARKER.len());
            self.bytes.extend_from_slice(TRUNCATION_MARKER);
            self.truncated = true;
        }
        let end = complete_utf8_prefix(&self.bytes);
        let start = self.emitted;
        self.emitted = end;
        self.bytes.get(start..end).unwrap_or_default()
    }

    pub(super) fn finish(&mut self) -> &[u8] {
        let start = self.emitted;
        self.emitted = self.bytes.len();
        self.bytes.get(start..).unwrap_or_default()
    }

    pub(super) fn into_bytes(self) -> Vec<u8> {
        self.bytes
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

    #[test]
    fn output_is_bounded_and_exactly_full_output_is_not_marked_truncated() {
        let full = vec![b'x'; MAX_CAPTURE_BYTES];
        assert_eq!(capture([full.clone()]), full);
        let captured = capture([full.clone(), vec![b'x'; MAX_CAPTURE_BYTES], vec![b'y'; 100]]);
        assert!(captured.starts_with(&full));
        assert!(captured.ends_with(TRUNCATION_MARKER));
        assert_eq!(captured.len(), MAX_CAPTURE_BYTES + TRUNCATION_MARKER.len());
    }

    #[test]
    fn truncation_keeps_unicode_valid_at_each_byte_boundary() {
        for omitted in 1..4 {
            let prefix = vec![b'x'; MAX_CAPTURE_BYTES - omitted];
            let character = "🦀".as_bytes();
            let mut chunks = vec![prefix.clone()];
            chunks.extend(character.iter().map(|byte| vec![*byte]));
            let captured = capture(chunks);
            assert!(std::str::from_utf8(&captured).is_ok());
            assert_eq!(captured.len(), prefix.len() + TRUNCATION_MARKER.len());
        }
    }

    #[test]
    fn untruncated_binary_and_split_unicode_are_preserved() {
        for bytes in ["a🦀z".as_bytes(), &[0xff, 0x00, 0xe2, 0x82]] {
            assert_eq!(capture(bytes.iter().map(|byte| vec![*byte])), bytes);
        }
    }
}
