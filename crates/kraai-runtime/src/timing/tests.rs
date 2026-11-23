use super::*;

#[test]
fn immediate_pause_and_finish_preserve_zero_duration_turns() {
    let now = Instant::now();
    let mut timer = TurnTimer::default();
    timer.start(now);
    timer.pause(now);
    assert_eq!(timer.elapsed(now), Some(Duration::ZERO));
    timer.finish(now);
    assert_eq!(timer.last_duration(), Some(Duration::ZERO));
}

#[test]
fn running_timer_round_trips_with_elapsed_time() {
    let now = Instant::now();
    let mut timer = TurnTimer::default();
    timer.start(now);
    let restored: TurnTimer =
        serde_json::from_str(&serde_json::to_string(&timer).unwrap()).unwrap();
    let checked = Instant::now() + Duration::from_secs(60);
    let elapsed = restored.elapsed(checked);
    assert!(elapsed >= Some(Duration::from_secs(59)));
    assert!(elapsed < Some(checked.duration_since(now) + Duration::from_secs(2)));
}

#[test]
fn approval_wait_is_excluded_and_completion_survives_repeated_finish() {
    let start = Instant::now();
    let mut timer = TurnTimer::default();
    timer.start(start);
    timer.pause(start + Duration::from_secs(5));
    assert_eq!(
        timer.elapsed(start + Duration::from_secs(100)),
        Some(Duration::from_secs(5))
    );
    timer.resume(start + Duration::from_secs(100));
    timer.resume(start + Duration::from_secs(101));
    timer.finish(start + Duration::from_secs(107));
    timer.finish(start + Duration::from_secs(110));
    assert_eq!(timer.last_duration(), Some(Duration::from_secs(12)));
    assert_eq!(timer.elapsed(start + Duration::from_secs(120)), None);
    timer.start(start + Duration::from_secs(130));
    assert_eq!(timer.last_duration(), None);
    assert_eq!(
        timer.elapsed(start + Duration::from_secs(132)),
        Some(Duration::from_secs(2))
    );
}
