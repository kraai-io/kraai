use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::Value;

pub(crate) const EXHAUSTED_ERRORS: [&str; 4] = [
    "insufficient_quota",
    "usage_not_included",
    "credit_balance_exhausted",
    "organization_usage_limit_exceeded",
];

const RESET_HEADERS: [(&str, &str); 2] = [
    (
        "x-ratelimit-remaining-requests",
        "x-ratelimit-reset-requests",
    ),
    ("x-ratelimit-remaining-tokens", "x-ratelimit-reset-tokens"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RateLimit {
    Exhausted,
    ResetsIn(Duration),
    Unknown,
}

pub(crate) fn classify(headers: &HeaderMap, body: &str) -> RateLimit {
    let body = serde_json::from_str::<Value>(body).ok();
    let error = body.as_ref().and_then(|body| body.get("error"));
    let field = |key: &str| {
        error
            .and_then(|error| error.get(key))
            .and_then(Value::as_str)
    };
    if [field("type"), field("code")]
        .into_iter()
        .flatten()
        .any(|kind| EXHAUSTED_ERRORS.contains(&kind))
    {
        return RateLimit::Exhausted;
    }
    if field("type") == Some("usage_limit_reached") {
        return error
            .and_then(usage_limit_reset)
            .or_else(|| retry_after(headers))
            .map_or(RateLimit::Exhausted, RateLimit::ResetsIn);
    }
    retry_after(headers)
        .or_else(|| rate_limit_reset(headers))
        .map_or(RateLimit::Unknown, RateLimit::ResetsIn)
}

pub(crate) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    if let Some(millis) = header(headers, "retry-after-ms").and_then(|raw| raw.parse::<f64>().ok())
    {
        return Duration::try_from_secs_f64(millis / 1000.0).ok();
    }
    let raw = header(headers, RETRY_AFTER.as_str())?;
    if let Ok(seconds) = raw.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let retry_at = httpdate::parse_http_date(raw).ok()?;
    Some(
        retry_at
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO),
    )
}

fn usage_limit_reset(error: &Value) -> Option<Duration> {
    if let Some(resets_at) = error.get("resets_at").and_then(Value::as_u64) {
        return Some(
            (UNIX_EPOCH + Duration::from_secs(resets_at))
                .duration_since(SystemTime::now())
                .unwrap_or(Duration::ZERO),
        );
    }
    error
        .get("resets_in_seconds")
        .and_then(Value::as_u64)
        .map(Duration::from_secs)
}

fn rate_limit_reset(headers: &HeaderMap) -> Option<Duration> {
    RESET_HEADERS
        .into_iter()
        .filter(|(remaining, _)| header(headers, remaining).is_none_or(|value| value == "0"))
        .filter_map(|(_, reset)| header(headers, reset).and_then(parse_reset))
        .max()
}

fn parse_reset(raw: &str) -> Option<Duration> {
    let mut rest = raw;
    let mut seconds = 0.0;
    while !rest.is_empty() {
        let number_end = rest
            .find(|character: char| !character.is_ascii_digit() && character != '.')
            .unwrap_or(rest.len());
        let unit_end = rest[number_end..]
            .find(|character: char| character.is_ascii_digit())
            .map_or(rest.len(), |offset| number_end + offset);
        let value = rest[..number_end].parse::<f64>().ok()?;
        let scale = match &rest[number_end..unit_end] {
            "ms" => 0.001,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return None,
        };
        seconds += value * scale;
        rest = &rest[unit_end..];
    }
    Duration::try_from_secs_f64(seconds).ok()
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn describe(delay: Duration) -> String {
    let seconds = delay.as_secs();
    let (days, hours, minutes) = (seconds / 86_400, seconds / 3600 % 24, seconds / 60 % 60);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{minutes}m {}s", seconds % 60),
        3600..86_400 => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

#[cfg(test)]
mod tests;
