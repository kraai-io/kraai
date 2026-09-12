use std::time::Duration;

pub(super) fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let tenths = duration.subsec_millis() / 100;
    let fraction = if tenths == 0 {
        String::new()
    } else {
        format!(".{tenths}")
    };
    if seconds < 60 {
        format!("{seconds}{fraction}s")
    } else if seconds < 3600 {
        format!("{}m{:02}{fraction}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{:02}m", seconds / 3600, (seconds / 60) % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_use_one_decimal_and_roll_over_to_minutes_and_hours() {
        for (millis, expected) in [
            (0, "0s"),
            (125, "0.1s"),
            (1999, "1.9s"),
            (59999, "59.9s"),
            (60000, "1m00s"),
            (61125, "1m01.1s"),
            (3600000, "1h00m"),
        ] {
            assert_eq!(format_duration(Duration::from_millis(millis)), expected);
        }
    }
}
