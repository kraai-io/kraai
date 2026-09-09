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
}

impl RuntimeEventSender {
    pub(crate) fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            state: Arc::new(Mutex::new(EventState { tx, sequence: 0 })),
        }
    }

    pub(crate) fn send(&self, event: Event) {
        // Assign and publish under one lock so concurrent producers cannot reorder events.
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.sequence += 1;
        let _ = state.tx.send(RuntimeEvent {
            sequence: state.sequence,
            event,
        });
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .tx
            .subscribe()
    }

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
