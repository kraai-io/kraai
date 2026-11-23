use std::time::{Duration, Instant, SystemTime};

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TurnTimer {
    #[cfg_attr(
        feature = "typescript",
        ts(type = "{ secs_since_epoch: number; nanos_since_epoch: number } | null")
    )]
    #[serde(with = "instant_serde")]
    started_at: Option<Instant>,
    #[cfg_attr(feature = "typescript", ts(type = "{ secs: number; nanos: number }"))]
    accumulated: Duration,
    active: bool,
    #[cfg_attr(
        feature = "typescript",
        ts(type = "{ secs: number; nanos: number } | null")
    )]
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

pub(crate) fn instant_at(time: SystemTime) -> Instant {
    let now = Instant::now();
    now.checked_sub(SystemTime::now().duration_since(time).unwrap_or_default())
        .unwrap_or(now)
}

#[cfg(test)]
mod tests;

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
        Ok(Option::<SystemTime>::deserialize(deserializer)?.map(super::instant_at))
    }
}
