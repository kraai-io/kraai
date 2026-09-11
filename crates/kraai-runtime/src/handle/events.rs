use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::{Event, RuntimeEvent};

#[derive(Clone)]
pub(crate) struct RuntimeEventSender {
    state: Arc<Mutex<EventState>>,
}

struct EventState {
    tx: broadcast::Sender<RuntimeEvent>,
    sequence: u64,
    timers: std::collections::HashMap<String, crate::TurnTimer>,
}

impl RuntimeEventSender {
    pub(crate) fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            state: Arc::new(Mutex::new(EventState {
                tx,
                sequence: 0,
                timers: Default::default(),
            })),
        }
    }

    pub(crate) fn send(&self, event: Event) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if matches!(
            &event,
            Event::StreamStart { .. }
                | Event::ScriptApprovalRequested { .. }
                | Event::StreamError { .. }
                | Event::StreamCancelled { .. }
                | Event::ContinuationFailed { .. }
        ) && let Some(session_id) = event.session_id()
        {
            let now = std::time::Instant::now();
            let timer = state.timers.entry(session_id.to_string()).or_default();
            let previous = *timer;
            match &event {
                Event::StreamStart { .. } => timer.resume(now),
                Event::ScriptApprovalRequested { .. } => timer.pause(now),
                Event::StreamError { .. }
                | Event::StreamCancelled { .. }
                | Event::ContinuationFailed { .. } => timer.finish(now),
                _ => {}
            }
            if *timer != previous {
                let timing = Event::TurnTimingChanged {
                    session_id: session_id.to_string(),
                    timer: *timer,
                };
                Self::publish(&mut state, timing);
            }
        }
        Self::publish(&mut state, event);
        drop(state);
    }

    fn publish(state: &mut EventState, event: Event) {
        state.sequence += 1;
        let _ = state.tx.send(RuntimeEvent {
            sequence: state.sequence,
            event,
        });
    }

    pub(crate) fn finish_timer(&self, session_id: &str) {
        self.update_timer(session_id, crate::TurnTimer::finish);
    }

    pub(crate) fn resume_timer(&self, session_id: &str) {
        self.update_timer(session_id, crate::TurnTimer::resume);
    }

    fn update_timer(
        &self,
        session_id: &str,
        update: fn(&mut crate::TurnTimer, std::time::Instant),
    ) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let timer = state.timers.entry(session_id.to_string()).or_default();
        let now = std::time::Instant::now();
        update(timer, now);
        let event = Event::TurnTimingChanged {
            session_id: session_id.to_string(),
            timer: *timer,
        };
        Self::publish(&mut state, event);
        drop(state);
    }

    pub(crate) fn timer_snapshot(&self, session_id: &str) -> (u64, crate::TurnTimer) {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        (
            state.sequence,
            state.timers.get(session_id).copied().unwrap_or_default(),
        )
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .tx
            .subscribe()
    }

    #[cfg(test)]
    pub(crate) fn latest_sequence(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sequence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_timing_survives_detaching_and_other_session_activity() {
        let sender = RuntimeEventSender::new(16);
        let receiver = sender.subscribe();
        sender.send(Event::StreamStart {
            session_id: "first".into(),
            message_id: "message".into(),
        });
        let (_, first) = sender.timer_snapshot("first");
        drop(receiver);
        sender.send(Event::StreamStart {
            session_id: "second".into(),
            message_id: "other".into(),
        });
        sender.send(Event::StreamCancelled {
            session_id: "second".into(),
            message_id: "other".into(),
        });
        let _receiver = sender.subscribe();
        assert_eq!(sender.timer_snapshot("first").1, first);
        assert!(first.elapsed(std::time::Instant::now()).is_some());
        sender.finish_timer("first");
        let finished = sender.timer_snapshot("first").1;
        assert!(finished.elapsed(std::time::Instant::now()).is_none());
        assert!(finished.last_duration().is_some());
    }

    #[test]
    fn concurrent_producers_publish_in_sequence_order() {
        const PRODUCERS: usize = 8;
        const EVENTS_PER_PRODUCER: usize = 10_000;
        let sender = RuntimeEventSender::new(PRODUCERS * EVENTS_PER_PRODUCER);
        let mut receiver = sender.subscribe();
        let start = std::sync::Barrier::new(PRODUCERS);
        std::thread::scope(|scope| {
            for _ in 0..PRODUCERS {
                let sender = sender.clone();
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    for _ in 0..EVENTS_PER_PRODUCER {
                        sender.send(Event::ConfigLoaded);
                    }
                });
            }
        });
        for expected in 1..=(PRODUCERS * EVENTS_PER_PRODUCER) as u64 {
            assert_eq!(
                receiver.try_recv().map(|event| event.sequence),
                Ok(expected)
            );
        }
        assert_eq!(
            sender.latest_sequence(),
            (PRODUCERS * EVENTS_PER_PRODUCER) as u64
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }
}
