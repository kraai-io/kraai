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
        let update: Option<fn(&mut crate::TurnTimer, std::time::Instant)> = match &event {
            Event::StreamStart { .. } => Some(crate::TurnTimer::resume),
            Event::ScriptApprovalRequested { .. } => Some(crate::TurnTimer::pause),
            Event::TurnCompleted { .. }
            | Event::ScriptPrepared { .. }
            | Event::ScriptStarted { .. }
            | Event::ScriptOutputTail { .. }
            | Event::RequestUsageUpdated { .. }
            | Event::TurnTimingChanged { .. }
            | Event::ConfigLoaded
            | Event::ModelsUpdated
            | Event::ServiceError { .. }
            | Event::SessionError { .. }
            | Event::ItemStarted { .. }
            | Event::ItemDelta { .. }
            | Event::ReasoningDelta { .. }
            | Event::StreamComplete { .. }
            | Event::StreamError { .. }
            | Event::StreamCancelled { .. }
            | Event::ProviderRetryScheduled { .. }
            | Event::StreamRestarted { .. }
            | Event::ScriptResultReady { .. }
            | Event::ContextStateChanged { .. }
            | Event::ContinuationFailed { .. }
            | Event::SessionStateChanged { .. }
            | Event::MessageUpserted { .. }
            | Event::MessagesRemoved { .. }
            | Event::HistoryUpdated { .. }
            | Event::ProviderAuthUpdated { .. }
            | Event::McpAuthUpdated { .. } => None,
        };
        if let Some(update) = update
            && let Some(session_id) = event.session_id()
        {
            let now = std::time::Instant::now();
            let timer = state.timers.entry(session_id.clone()).or_default();
            let previous = *timer;
            update(timer, now);
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
            Event::ScriptPrepared { .. }
            | Event::ScriptStarted { .. }
            | Event::ScriptOutputTail { .. }
            | Event::RequestUsageUpdated { .. }
            | Event::TurnTimingChanged { .. }
            | Event::ConfigLoaded
            | Event::ModelsUpdated
            | Event::ServiceError { .. }
            | Event::ItemStarted { .. }
            | Event::ItemDelta { .. }
            | Event::ReasoningDelta { .. }
            | Event::StreamComplete { .. }
            | Event::ProviderRetryScheduled { .. }
            | Event::StreamRestarted { .. }
            | Event::ScriptApprovalRequested { .. }
            | Event::ScriptResultReady { .. }
            | Event::ContextStateChanged { .. }
            | Event::SessionStateChanged { .. }
            | Event::MessageUpserted { .. }
            | Event::MessagesRemoved { .. }
            | Event::HistoryUpdated { .. }
            | Event::ProviderAuthUpdated { .. }
            | Event::McpAuthUpdated { .. } => return,
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

    pub(crate) fn has_session_subscribers(&self, session_id: &SessionId) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sessions
            .get(session_id)
            .is_some_and(|sender| sender.receiver_count() > 0)
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
mod tests;
