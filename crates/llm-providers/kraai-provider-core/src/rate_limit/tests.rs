use super::*;
use reqwest::header::{HeaderName, HeaderValue};

fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
    pairs
        .iter()
        .map(|(name, value)| {
            (
                HeaderName::from_static(name),
                HeaderValue::from_str(value).unwrap(),
            )
        })
        .collect()
}

#[test]
fn quota_errors_without_a_reset_are_exhausted() {
    for body in [
        r#"{"error":{"type":"insufficient_quota","code":"insufficient_quota","message":"You exceeded your current quota"}}"#,
        r#"{"error":{"type":"usage_not_included"}}"#,
        r#"{"error":{"code":"credit_balance_exhausted"}}"#,
        r#"{"error":{"type":"usage_limit_reached","plan_type":"plus"}}"#,
    ] {
        assert_eq!(
            classify(&headers(&[("retry-after", "5")]), body),
            if body.contains("usage_limit_reached") {
                RateLimit::ResetsIn(Duration::from_secs(5))
            } else {
                RateLimit::Exhausted
            },
            "{body}"
        );
        assert_eq!(classify(&HeaderMap::new(), body), RateLimit::Exhausted);
    }
}

#[test]
fn usage_limits_wait_until_their_reset() {
    let resets_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 7200;
    let RateLimit::ResetsIn(delay) = classify(
        &headers(&[("retry-after", "1")]),
        &format!(r#"{{"error":{{"type":"usage_limit_reached","resets_at":{resets_at}}}}}"#),
    ) else {
        panic!("usage limit must carry its reset");
    };
    assert!(delay > Duration::from_secs(7190) && delay <= Duration::from_secs(7200));
    assert_eq!(
        classify(
            &HeaderMap::new(),
            r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":90}}"#
        ),
        RateLimit::ResetsIn(Duration::from_secs(90))
    );
    assert_eq!(
        classify(
            &HeaderMap::new(),
            r#"{"error":{"type":"usage_limit_reached","resets_at":1}}"#
        ),
        RateLimit::ResetsIn(Duration::ZERO)
    );
}

#[test]
fn retry_headers_set_the_reset() {
    for (pairs, expected) in [
        (
            vec![("retry-after-ms", "1500.5"), ("retry-after", "9")],
            RateLimit::ResetsIn(Duration::from_micros(1_500_500)),
        ),
        (
            vec![("retry-after", "9")],
            RateLimit::ResetsIn(Duration::from_secs(9)),
        ),
        (
            vec![
                ("x-ratelimit-remaining-requests", "0"),
                ("x-ratelimit-reset-requests", "1s"),
                ("x-ratelimit-remaining-tokens", "12"),
                ("x-ratelimit-reset-tokens", "6m0s"),
            ],
            RateLimit::ResetsIn(Duration::from_secs(1)),
        ),
        (
            vec![
                ("x-ratelimit-reset-requests", "20ms"),
                ("x-ratelimit-reset-tokens", "1h2m3.5s"),
            ],
            RateLimit::ResetsIn(Duration::from_millis(3_723_500)),
        ),
        (
            vec![
                ("x-ratelimit-remaining-requests", "3"),
                ("x-ratelimit-reset-requests", "1s"),
            ],
            RateLimit::Unknown,
        ),
        (
            vec![("x-ratelimit-reset-tokens", "soon")],
            RateLimit::Unknown,
        ),
        (vec![("retry-after", "later")], RateLimit::Unknown),
        (vec![], RateLimit::Unknown),
    ] {
        assert_eq!(
            classify(&headers(&pairs), "slow down"),
            expected,
            "{pairs:?}"
        );
    }
}

#[test]
fn delays_are_described_in_readable_units() {
    for (seconds, expected) in [
        (0, "0s"),
        (59, "59s"),
        (61, "1m 1s"),
        (7_260, "2h 1m"),
        (90_000, "1d 1h"),
    ] {
        assert_eq!(describe(Duration::from_secs(seconds)), expected);
    }
}
