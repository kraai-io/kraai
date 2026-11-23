use super::{age, compact_tokens};

#[test]
fn ages_use_the_largest_whole_unit() {
    for (seconds, expected) in [
        (0, " 0s"),
        (59, "59s"),
        (60, " 1m"),
        (3599, "59m"),
        (3600, " 1h"),
        (86399, "23h"),
        (86400, " 1d"),
        (86400 * 400, "400d"),
    ] {
        assert_eq!(age(seconds), expected);
    }
}

#[test]
fn context_windows_shorten_to_thousands_and_millions() {
    assert_eq!(compact_tokens(900), "900");
    assert_eq!(compact_tokens(200_000), "200k");
    assert_eq!(compact_tokens(1_000_000), "1M");
    assert_eq!(compact_tokens(1_050_000), "1M");
    assert_eq!(compact_tokens(2_500_000), "2.5M");
}
