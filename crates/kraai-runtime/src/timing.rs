use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TurnTimer {
    #[serde(with = "instant_serde")]
    started_at: Option<Instant>,
    accumulated: Duration,
    active: bool,
    last_duration: Option<Duration>,
}

impl TurnTimer {
    pub(crate) fn start(&mut self, now: Instant) {
        self.active = true;
        self.started_at = Some(now);
        self.accumulated = Duration::ZERO;
        self.last_duration = None;
    }

    pub(crate) fn resume(&mut self, now: Instant) {
        if !self.active {
            self.start(now);
        } else if self.started_at.is_none() {
            self.started_at = Some(now);
        }
    }

    pub(crate) fn pause(&mut self, now: Instant) {
        if let Some(started_at) = self.started_at.take() {
            self.accumulated = self
                .accumulated
                .saturating_add(now.saturating_duration_since(started_at));
        }
    }

    pub(crate) fn finish(&mut self, now: Instant) {
        self.pause(now);
        if self.active {
            self.last_duration = Some(self.accumulated);
        }
        self.accumulated = Duration::ZERO;
        self.active = false;
    }

    pub fn elapsed(&self, now: Instant) -> Option<Duration> {
        self.active.then(|| {
            self.accumulated.saturating_add(
                self.started_at
                    .map(|started_at| now.saturating_duration_since(started_at))
                    .unwrap_or_default(),
            )
        })
    }

    pub fn last_duration(&self) -> Option<Duration> {
        self.last_duration
    }
}

#[cfg(test)]
mod tests {
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
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions follow fallible fixture setup"
    )]
    fn running_timer_round_trips_with_elapsed_time() -> serde_json::Result<()> {
        let now = Instant::now();
        let mut timer = TurnTimer::default();
        timer.start(now - Duration::from_secs(60));
        let restored: TurnTimer = serde_json::from_str(&serde_json::to_string(&timer)?)?;
        let elapsed = restored.elapsed(Instant::now());
        assert!(elapsed >= Some(Duration::from_secs(59)));
        assert!(elapsed < Some(Duration::from_secs(62)));
        Ok(())
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
}

mod instant_serde {
    use std::time::{Instant, SystemTime};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(
        value: &Option<Instant>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let now = Instant::now();
        let wall_time = SystemTime::now();
        value
            .and_then(|started| wall_time.checked_sub(now.saturating_duration_since(started)))
            .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Instant>, D::Error> {
        let value = Option::<SystemTime>::deserialize(deserializer)?;
        let now = Instant::now();
        let wall_time = SystemTime::now();
        Ok(value.map(|started| {
            now.checked_sub(wall_time.duration_since(started).unwrap_or_default())
                .unwrap_or(now)
        }))
    }
}
