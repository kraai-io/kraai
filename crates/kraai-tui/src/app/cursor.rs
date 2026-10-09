use std::time::{Duration, Instant};

use super::{AppState, UiMode};

pub(super) struct CursorBlink {
    epoch: Instant,
    pub(super) visible: bool,
}

impl CursorBlink {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            epoch: now,
            visible: true,
        }
    }

    pub(super) fn update(&mut self, now: Instant, input: bool) -> bool {
        if input {
            self.epoch = now;
        }
        let visible = (now.duration_since(self.epoch).as_millis() / 500).is_multiple_of(2);
        let changed = visible != self.visible;
        self.visible = visible;
        changed
    }

    pub(super) fn timeout(&self, now: Instant) -> Duration {
        Duration::from_millis(500 - (now.duration_since(self.epoch).as_millis() % 500) as u64)
    }
}

impl AppState {
    pub(super) fn composer_cursor_enabled(&self) -> bool {
        self.mode == UiMode::Chat
            && !(self.error_open && self.last_error.is_some())
            && !self.has_local_script_approval()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redraws_do_not_restart_blink_but_input_does() {
        let start = Instant::now();
        let mut cursor = CursorBlink::new(start);
        for millis in 0..500 {
            assert!(!cursor.update(start + Duration::from_millis(millis), false));
            assert!(cursor.visible);
        }
        assert!(cursor.update(start + Duration::from_millis(500), false));
        assert!(!cursor.visible);
        assert!(!cursor.update(start + Duration::from_millis(700), false));
        assert_eq!(
            cursor.timeout(start + Duration::from_millis(700)),
            Duration::from_millis(300)
        );
        assert!(cursor.update(start + Duration::from_millis(750), true));
        assert!(cursor.visible);
        assert!(!cursor.update(start + Duration::from_millis(1000), false));
        assert!(cursor.update(start + Duration::from_millis(1250), false));
        assert!(!cursor.visible);
        assert!(cursor.update(start + Duration::from_millis(1750), false));
        assert!(cursor.visible);
    }
}
