use super::*;

#[test]
fn runtime_changes_draw_at_most_once_per_frame() {
    let start = Instant::now();
    let mut pacer = FramePacer::default();
    assert_eq!(pacer.timeout(start), None);
    assert!(!pacer.due(start));

    pacer.request(true);
    assert!(pacer.due(start));
    assert_eq!(pacer.timeout(start), Some(Duration::ZERO));
    pacer.drawn(start);
    assert!(!pacer.due(start));

    let soon = start + Duration::from_millis(4);
    pacer.request(false);
    assert_eq!(pacer.timeout(soon), None);
    pacer.request(true);
    pacer.request(true);
    assert!(!pacer.due(soon));
    assert_eq!(pacer.timeout(soon), Some(Duration::from_millis(12)));

    let later = start + FRAME_INTERVAL;
    assert!(pacer.due(later));
    assert_eq!(pacer.timeout(later), Some(Duration::ZERO));
    pacer.drawn(later);
    assert_eq!(pacer.timeout(later), None);
}
