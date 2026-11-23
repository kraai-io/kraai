use super::parse_duration;
use std::time::Duration;

#[test]
fn parses_pinned_nushell_duration_units_and_fractions() {
    assert_eq!(parse_duration("30sec"), Ok(Duration::from_secs(30)));
    assert_eq!(parse_duration("10min"), Ok(Duration::from_secs(600)));
    assert_eq!(parse_duration("1.5sec"), Ok(Duration::from_millis(1500)));
    assert_eq!(parse_duration("250ms"), Ok(Duration::from_millis(250)));
    assert_eq!(parse_duration("10s"), Ok(Duration::from_secs(10)));
    assert_eq!(parse_duration("2m"), Ok(Duration::from_secs(120)));
    assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
}

#[test]
fn rejects_zero_unknown_and_sub_nanosecond_durations() {
    assert!(parse_duration("0sec").is_err());
    assert!(parse_duration("10secs").is_err());
    assert!(parse_duration("0.1ns").is_err());
    assert!(parse_duration("-1sec").is_err());
}
