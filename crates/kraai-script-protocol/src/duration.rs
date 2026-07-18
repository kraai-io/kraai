use std::time::Duration;

use crate::ProtocolError;

pub(crate) fn parse_duration(input: &str) -> Result<Duration, ProtocolError> {
    let unit_start = input
        .bytes()
        .position(|byte| !byte.is_ascii_digit() && byte != b'.')
        .ok_or_else(|| ProtocolError::InvalidTimeout(String::from("missing duration unit")))?;
    let (number, unit) = input.split_at(unit_start);
    if number.is_empty() || unit.is_empty() {
        return Err(ProtocolError::InvalidTimeout(String::from(
            "expected a positive number followed by ns, us, ms, sec, min, hr, day, or wk",
        )));
    }
    let (whole, fraction) = match number.split_once('.') {
        Some((whole, fraction))
            if !whole.is_empty()
                && !fraction.is_empty()
                && !fraction.contains('.')
                && whole.bytes().all(|byte| byte.is_ascii_digit())
                && fraction.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            (whole, Some(fraction))
        }
        None if whole_number(number) => (number, None),
        _ => {
            return Err(ProtocolError::InvalidTimeout(String::from(
                "duration number must be an unsigned decimal",
            )));
        }
    };
    let nanos_per_unit = match unit {
        "ns" => 1_u128,
        "us" | "µs" => 1_000,
        "ms" => 1_000_000,
        "sec" => 1_000_000_000,
        "min" => 60 * 1_000_000_000,
        "hr" => 60 * 60 * 1_000_000_000,
        "day" => 24 * 60 * 60 * 1_000_000_000,
        "wk" => 7 * 24 * 60 * 60 * 1_000_000_000,
        _ => {
            return Err(ProtocolError::InvalidTimeout(format!(
                "unknown Nushell duration unit '{unit}'"
            )));
        }
    };
    let whole = whole
        .parse::<u128>()
        .map_err(|error| ProtocolError::InvalidTimeout(error.to_string()))?;
    let mut nanos = whole
        .checked_mul(nanos_per_unit)
        .ok_or_else(|| ProtocolError::InvalidTimeout(String::from("duration overflow")))?;
    if let Some(fraction) = fraction {
        let scale = 10_u128
            .checked_pow(u32::try_from(fraction.len()).map_err(|error| {
                ProtocolError::InvalidTimeout(format!("fraction is too precise: {error}"))
            })?)
            .ok_or_else(|| ProtocolError::InvalidTimeout(String::from("fraction overflow")))?;
        let fraction = fraction
            .parse::<u128>()
            .map_err(|error| ProtocolError::InvalidTimeout(error.to_string()))?;
        let scaled = fraction
            .checked_mul(nanos_per_unit)
            .ok_or_else(|| ProtocolError::InvalidTimeout(String::from("duration overflow")))?;
        if scaled % scale != 0 {
            return Err(ProtocolError::InvalidTimeout(String::from(
                "duration is more precise than one nanosecond",
            )));
        }
        nanos = nanos
            .checked_add(scaled / scale)
            .ok_or_else(|| ProtocolError::InvalidTimeout(String::from("duration overflow")))?;
    }
    if nanos == 0 {
        return Err(ProtocolError::InvalidTimeout(String::from(
            "duration must be greater than zero",
        )));
    }
    let seconds = u64::try_from(nanos / 1_000_000_000)
        .map_err(|error| ProtocolError::InvalidTimeout(format!("duration overflow: {error}")))?;
    let subsecond_nanos = u32::try_from(nanos % 1_000_000_000)
        .map_err(|error| ProtocolError::InvalidTimeout(format!("duration overflow: {error}")))?;
    Ok(Duration::new(seconds, subsecond_nanos))
}

fn whole_number(number: &str) -> bool {
    !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::parse_duration;
    use std::time::Duration;

    #[test]
    fn parses_pinned_nushell_duration_units_and_fractions() {
        assert_eq!(parse_duration("30sec"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_duration("10min"), Ok(Duration::from_secs(600)));
        assert_eq!(parse_duration("1.5sec"), Ok(Duration::from_millis(1500)));
        assert_eq!(parse_duration("250ms"), Ok(Duration::from_millis(250)));
    }

    #[test]
    fn rejects_zero_unknown_and_sub_nanosecond_durations() {
        assert!(parse_duration("0sec").is_err());
        assert!(parse_duration("10s").is_err());
        assert!(parse_duration("0.1ns").is_err());
        assert!(parse_duration("-1sec").is_err());
    }
}
