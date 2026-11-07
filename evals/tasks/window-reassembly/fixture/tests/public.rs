use window_reassembly::{Reassembler, ReassemblyError};

#[test]
fn reorders_within_the_window() {
    let mut reassembler = Reassembler::new(10, 4).unwrap();
    assert_eq!(reassembler.push(12, b"c".to_vec()), Ok(vec![]));
    assert_eq!(reassembler.push(11, b"b".to_vec()), Ok(vec![]));
    assert_eq!(reassembler.buffered(), 2);
    assert_eq!(
        reassembler.push(10, b"a".to_vec()),
        Ok(vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()])
    );
    assert_eq!(reassembler.expected(), 13);
    assert_eq!(
        reassembler.push(17, b"far".to_vec()),
        Err(ReassemblyError::OutsideWindow(17))
    );
}

/// Sends packets in blocks of five, each block reversed, and traces every delivery so a
/// failure shows how far the stream got.
#[test]
fn long_reordered_stream() {
    const PACKETS: u64 = 70_000;
    let mut reassembler = Reassembler::new(0, 64).unwrap();
    let mut next = 0;
    for block in (0..PACKETS).step_by(5) {
        for index in (block..(block + 5).min(PACKETS)).rev() {
            let seq = index as u16;
            let delivered = reassembler.push(seq, index.to_le_bytes().to_vec()).unwrap();
            for payload in delivered {
                let packet = u64::from_le_bytes(payload.try_into().unwrap());
                println!(
                    "delivered packet {packet:>6} seq {:>5} expected {:>5} buffered {}",
                    packet as u16,
                    reassembler.expected(),
                    reassembler.buffered()
                );
                assert_eq!(packet, next, "packet {packet} arrived before packet {next}");
                next += 1;
            }
        }
    }
    assert_eq!(
        next,
        PACKETS,
        "stream stalled after {next} packets, expecting seq {} with {} buffered",
        reassembler.expected(),
        reassembler.buffered()
    );
}
