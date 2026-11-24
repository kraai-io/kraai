use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Default)]
pub(super) struct FramePacer {
    pending: bool,
    last_frame: Option<Instant>,
}

impl FramePacer {
    pub(super) fn request(&mut self, changed: bool) {
        self.pending |= changed;
    }

    pub(super) fn timeout(&self, now: Instant) -> Option<Duration> {
        self.pending.then(|| {
            self.next_frame()
                .map_or(Duration::ZERO, |due| due.saturating_duration_since(now))
        })
    }

    pub(super) fn due(&self, now: Instant) -> bool {
        self.pending && self.next_frame().is_none_or(|due| now >= due)
    }

    pub(super) fn drawn(&mut self, at: Instant) {
        self.pending = false;
        self.last_frame = Some(at);
    }

    fn next_frame(&self) -> Option<Instant> {
        self.last_frame.map(|frame| frame + FRAME_INTERVAL)
    }
}

#[cfg(test)]
mod tests;
