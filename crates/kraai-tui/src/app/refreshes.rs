use std::collections::HashMap;

use kraai_runtime::RuntimeEvent;
use kraai_types::SessionId;

use super::{RuntimeRequest, RuntimeResponse};

#[derive(Default)]
struct Refresh {
    in_flight: bool,
    dirty: bool,
}

impl Refresh {
    fn start(&mut self) -> bool {
        if self.in_flight {
            self.dirty = true;
            return false;
        }
        self.in_flight = true;
        true
    }

    fn finish(&mut self) -> bool {
        self.in_flight = false;
        std::mem::take(&mut self.dirty)
    }
}

#[derive(Default)]
struct SnapshotSync {
    dirty: bool,
    buffered: Vec<RuntimeEvent>,
}

#[derive(Default)]
pub(super) struct Refreshes {
    sessions: Refresh,
    input_history: Refresh,
    snapshots: HashMap<SessionId, SnapshotSync>,
}

pub(super) enum FinishedRefresh {
    None,
    Repeat(RuntimeRequest),
    Snapshot {
        session_id: SessionId,
        dirty: bool,
        buffered: Vec<RuntimeEvent>,
    },
}

impl Refreshes {
    /// False when the same refresh is already in flight, which repeats it once it lands.
    pub(super) fn start(&mut self, request: &RuntimeRequest) -> bool {
        if matches!(request, RuntimeRequest::ListSessions) {
            return self.sessions.start();
        }
        if matches!(request, RuntimeRequest::ListUserInputHistory { .. }) {
            return self.input_history.start();
        }
        let RuntimeRequest::GetSessionSnapshot { session_id } = request else {
            return true;
        };
        if let Some(sync) = self.snapshots.get_mut(session_id) {
            sync.dirty = true;
            return false;
        }
        self.snapshots
            .insert(session_id.clone(), SnapshotSync::default());
        true
    }

    /// Hands back events while a snapshot of their session is in flight.
    pub(super) fn buffer(&mut self, event: RuntimeEvent) -> Option<RuntimeEvent> {
        let Some(sync) = event
            .event
            .session_id()
            .and_then(|session_id| self.snapshots.get_mut(session_id))
        else {
            return Some(event);
        };
        sync.buffered.push(event);
        None
    }

    pub(super) fn finish(&mut self, response: &RuntimeResponse) -> FinishedRefresh {
        let repeat = |refresh: &mut Refresh, request: RuntimeRequest| {
            if refresh.finish() {
                FinishedRefresh::Repeat(request)
            } else {
                FinishedRefresh::None
            }
        };
        if matches!(response, RuntimeResponse::Sessions(_)) {
            return repeat(&mut self.sessions, RuntimeRequest::ListSessions);
        }
        if matches!(response, RuntimeResponse::UserInputHistory(_)) {
            return repeat(
                &mut self.input_history,
                RuntimeRequest::ListUserInputHistory {
                    limit: super::INPUT_HISTORY_LIMIT,
                },
            );
        }
        let RuntimeResponse::SessionSnapshot { session_id, .. } = response else {
            return FinishedRefresh::None;
        };
        self.snapshots
            .remove(session_id)
            .map_or(FinishedRefresh::None, |sync| FinishedRefresh::Snapshot {
                session_id: session_id.clone(),
                dirty: sync.dirty,
                buffered: sync.buffered,
            })
    }
}

impl super::App {
    pub(super) fn route_runtime_event(&mut self, event: RuntimeEvent) {
        let Some(event) = self.refreshes.buffer(event) else {
            return;
        };
        let covered_by_snapshot = event.event.session_id().is_some_and(|session_id| {
            self.session_snapshot_sequences
                .get(session_id)
                .is_some_and(|snapshot_sequence| event.sequence <= *snapshot_sequence)
        });
        if covered_by_snapshot {
            self.apply_runtime_event(event.event, false);
        } else {
            self.handle_runtime_event(event.event);
        }
    }

    pub(super) fn continue_refresh(&mut self, finished: FinishedRefresh) {
        match finished {
            FinishedRefresh::None => {}
            FinishedRefresh::Repeat(request) => {
                self.request(request);
            }
            FinishedRefresh::Snapshot {
                session_id,
                dirty,
                buffered,
            } => {
                for event in buffered {
                    self.route_runtime_event(event);
                }
                if dirty {
                    self.request(RuntimeRequest::GetSessionSnapshot { session_id });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
