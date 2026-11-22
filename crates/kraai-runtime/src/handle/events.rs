use std::sync::{Arc, Mutex};

use kraai_types::SessionId;
use tokio::sync::broadcast;

use crate::{Event, RuntimeEvent};

pub(crate) struct TurnSnapshot {
    pub(crate) sequence: u64,
    pub(crate) timer: crate::TurnTimer,
    pub(crate) outcome: Option<crate::TurnOutcome>,
}

#[derive(Clone)]
pub(crate) struct RuntimeEventSender {
    state: Arc<Mutex<EventState>>,
}

struct EventState {
    tx: broadcast::Sender<RuntimeEvent>,
    sessions: std::collections::HashMap<SessionId, broadcast::Sender<RuntimeEvent>>,
    capacity: usize,
    sequence: u64,
    timers: std::collections::HashMap<SessionId, crate::TurnTimer>,
    outcomes: std::collections::HashMap<SessionId, crate::TurnOutcome>,
}

impl RuntimeEventSender {
    pub(crate) fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            state: Arc::new(Mutex::new(EventState {
                tx,
                sessions: Default::default(),
                capacity,
                sequence: 0,
                timers: Default::default(),
                outcomes: Default::default(),
            })),
        }
    }

    pub(crate) fn send(&self, event: Event) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        Self::record_outcome(&mut state, &event);
        if matches!(
            &event,
            Event::StreamStart { .. } | Event::ScriptApprovalRequested { .. }
        ) && let Some(session_id) = event.session_id()
        {
            let now = std::time::Instant::now();
            let timer = state.timers.entry(session_id.clone()).or_default();
            let previous = *timer;
            match &event {
                Event::StreamStart { .. } => timer.resume(now),
                Event::ScriptApprovalRequested { .. } => timer.pause(now),
                _ => {}
            }
            if *timer != previous {
                let timing = Event::TurnTimingChanged {
                    session_id: session_id.clone(),
                    timer: *timer,
                };
                Self::publish(&mut state, timing);
            }
        }
        Self::publish(&mut state, event);
        drop(state);
    }

    #[expect(
        clippy::iter_over_hash_type,
        reason = "service failures reach independent session receivers in any order"
    )]
    fn publish(state: &mut EventState, event: Event) {
        state.sequence += 1;
        let event = RuntimeEvent {
            sequence: state.sequence,
            event,
        };
        if let Some(session_id) = event.event.session_id() {
            if let Some(sender) = state.sessions.get(session_id) {
                let _ = sender.send(event.clone());
            }
        } else if matches!(event.event, Event::ServiceError { .. }) {
            for sender in state.sessions.values() {
                let _ = sender.send(event.clone());
            }
        }
        let _ = state.tx.send(event);
    }

    fn record_outcome(state: &mut EventState, event: &Event) {
        let Some(session_id) = event.session_id() else {
            return;
        };
        let outcome = match event {
            Event::StreamStart { .. } => {
                state.outcomes.remove(session_id);
                return;
            }
            Event::TurnCompleted { .. } => crate::TurnOutcome::Completed,
            Event::StreamCancelled { .. } => crate::TurnOutcome::Cancelled,
            Event::StreamError { error, .. } | Event::ContinuationFailed { error, .. } => {
                crate::TurnOutcome::Failed {
                    error: crate::RuntimeError::internal(error),
                }
            }
            Event::SessionError { error, .. } => crate::TurnOutcome::Failed {
                error: error.clone(),
            },
            _ => return,
        };
        state.outcomes.insert(session_id.clone(), outcome);
    }

    pub(crate) fn clear_outcome(&self, session_id: &SessionId) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .outcomes
            .remove(session_id);
    }

    pub(crate) fn forget_session(&self, session_id: &SessionId) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.timers.remove(session_id);
        state.outcomes.remove(session_id);
    }

    pub(crate) fn finish_timer(&self, session_id: &SessionId) {
        self.update_timer(session_id, crate::TurnTimer::finish);
    }

    pub(crate) fn resume_timer(&self, session_id: &SessionId) {
        self.update_timer(session_id, crate::TurnTimer::resume);
    }

    fn update_timer(
        &self,
        session_id: &SessionId,
        update: fn(&mut crate::TurnTimer, std::time::Instant),
    ) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let timer = state.timers.entry(session_id.clone()).or_default();
        let now = std::time::Instant::now();
        update(timer, now);
        let event = Event::TurnTimingChanged {
            session_id: session_id.clone(),
            timer: *timer,
        };
        Self::publish(&mut state, event);
        drop(state);
    }

    pub(crate) fn turn_snapshot(&self, session_id: &SessionId) -> TurnSnapshot {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        TurnSnapshot {
            sequence: state.sequence,
            timer: state.timers.get(session_id).copied().unwrap_or_default(),
            outcome: state.outcomes.get(session_id).cloned(),
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .tx
            .subscribe()
    }

    pub(crate) fn subscribe_session(
        &self,
        session_id: &SessionId,
    ) -> broadcast::Receiver<RuntimeEvent> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state
            .sessions
            .retain(|_, sender| sender.receiver_count() > 0);
        let capacity = state.capacity;
        state
            .sessions
            .entry(session_id.clone())
            .or_insert_with(|| broadcast::channel(capacity).0)
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
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions validate event isolation after fallible channel reads"
    )]
    fn scoped_subscribers_are_isolated_and_receive_service_failures()
    -> Result<(), broadcast::error::TryRecvError> {
        let sender = RuntimeEventSender::new(2);
        let mut first = sender.subscribe_session(&SessionId::new("first"));
        let mut second = sender.subscribe_session(&SessionId::new("second"));
        for _ in 0..1025 {
            sender.send(Event::ItemDelta {
                session_id: SessionId::new("second"),
                message_id: "message".into(),
                item_index: 0,
                delta: "text".into(),
            });
        }
        assert!(matches!(
            first.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            second.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
        sender.send(Event::ServiceError {
            error: crate::RuntimeError::unavailable("service stopped"),
        });
        assert!(matches!(
            first.try_recv()?.event,
            Event::ServiceError { .. }
        ));
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions validate event ordering after fallible channel reads"
    )]
    fn scoped_channels_preserve_order_and_release_detached_sessions()
    -> Result<(), broadcast::error::TryRecvError> {
        let sender = RuntimeEventSender::new(4);
        let mut first = sender.subscribe_session(&SessionId::new("first"));
        let mut global = sender.subscribe();
        sender.send(Event::ItemDelta {
            session_id: SessionId::new("first"),
            message_id: "message".into(),
            item_index: 0,
            delta: "text".into(),
        });
        assert_eq!(first.try_recv()?.sequence, global.try_recv()?.sequence);
        drop(first);
        let _second = sender.subscribe_session(&SessionId::new("second"));
        let state = sender
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(!state.sessions.contains_key(&SessionId::new("first")));
        assert!(state.sessions.contains_key(&SessionId::new("second")));
        drop(state);
        Ok(())
    }

    #[test]
    fn terminal_events_do_not_finish_a_turn_before_cleanup() {
        let sender = RuntimeEventSender::new(16);
        sender.resume_timer(&SessionId::new("session"));
        let timer = sender.turn_snapshot(&SessionId::new("session")).timer;
        for event in [
            Event::StreamError {
                session_id: SessionId::new("session"),
                message_id: "message".into(),
                error: "rollback failed".into(),
            },
            Event::ContinuationFailed {
                session_id: SessionId::new("session"),
                error: "rollback failed".into(),
            },
            Event::StreamCancelled {
                session_id: SessionId::new("session"),
                message_id: "message".into(),
            },
        ] {
            sender.send(event);
            assert_eq!(
                sender.turn_snapshot(&SessionId::new("session")).timer,
                timer
            );
        }
        sender.finish_timer(&SessionId::new("session"));
        assert!(
            sender
                .turn_snapshot(&SessionId::new("session"))
                .timer
                .elapsed(std::time::Instant::now())
                .is_none()
        );
    }

    #[test]
    fn turn_outcome_follows_the_latest_turn() {
        let sender = RuntimeEventSender::new(16);
        let session = SessionId::new("session");
        let outcome = || sender.turn_snapshot(&session).outcome;
        let start = || Event::StreamStart {
            session_id: session.clone(),
            message_id: "message".into(),
        };
        sender.send(Event::StreamError {
            session_id: session.clone(),
            message_id: "message".into(),
            error: "rate limited".into(),
        });
        assert!(matches!(
            outcome(),
            Some(crate::TurnOutcome::Failed { error }) if error.message == "rate limited"
        ));
        sender.send(start());
        assert_eq!(outcome(), None);
        sender.send(Event::TurnCompleted {
            session_id: session.clone(),
        });
        assert_eq!(outcome(), Some(crate::TurnOutcome::Completed));
        sender.send(start());
        sender.send(Event::StreamCancelled {
            session_id: session.clone(),
            message_id: "message".into(),
        });
        assert_eq!(outcome(), Some(crate::TurnOutcome::Cancelled));
        sender.clear_outcome(&session);
        assert_eq!(outcome(), None);
        sender.send(Event::TurnCompleted {
            session_id: session.clone(),
        });
        sender.forget_session(&session);
        assert_eq!(outcome(), None);
    }

    #[test]
    fn session_timing_survives_detaching_and_other_session_activity() {
        let sender = RuntimeEventSender::new(16);
        let receiver = sender.subscribe();
        sender.send(Event::StreamStart {
            session_id: SessionId::new("first"),
            message_id: "message".into(),
        });
        let first = sender.turn_snapshot(&SessionId::new("first")).timer;
        drop(receiver);
        sender.send(Event::StreamStart {
            session_id: SessionId::new("second"),
            message_id: "other".into(),
        });
        sender.send(Event::StreamCancelled {
            session_id: SessionId::new("second"),
            message_id: "other".into(),
        });
        let _receiver = sender.subscribe();
        assert_eq!(sender.turn_snapshot(&SessionId::new("first")).timer, first);
        assert!(first.elapsed(std::time::Instant::now()).is_some());
        sender.finish_timer(&SessionId::new("first"));
        let finished = sender.turn_snapshot(&SessionId::new("first")).timer;
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
