use std::time::Duration;

pub(crate) fn format_duration(duration: Duration) -> String {
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

pub(crate) fn format_limit(limit: Duration) -> String {
    let text = format_duration(limit);
    for (suffix, unit) in [("m00s", "m"), ("h00m", "h")] {
        if let Some(head) = text.strip_suffix(suffix) {
            return format!("{head}{unit}");
        }
    }
    text
}

#[cfg(test)]
mod tests;
