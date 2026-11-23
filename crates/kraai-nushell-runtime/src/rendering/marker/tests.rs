use super::{Marker, Scan};

#[test]
fn markers_round_trip_and_partial_markers_wait_for_more_bytes() {
    let marker = Marker::random();
    let encoded = marker.encode(42);
    assert!(matches!(
        marker.scan(&encoded),
        Scan::Marker { length, id: 42 } if length == encoded.len()
    ));
    for split in 1..encoded.len() {
        assert!(matches!(
            marker.scan(encoded.get(..split).unwrap_or_default()),
            Scan::Incomplete
        ));
    }
    assert!(matches!(marker.scan(b"text\x1e"), Scan::Text(4)));
}

#[test]
fn foreign_record_separators_stay_text() {
    let marker = Marker::random();
    let other = Marker::random().encode(1);
    assert!(matches!(marker.scan(&other), Scan::Text(1)));
    let mut broken = marker.encode(1);
    broken.pop();
    broken.push(b'x');
    assert!(matches!(marker.scan(&broken), Scan::Text(1)));
    assert!(matches!(marker.scan(b"\x1e!x"), Scan::Text(1)));
}
