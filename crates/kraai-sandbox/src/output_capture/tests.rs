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
