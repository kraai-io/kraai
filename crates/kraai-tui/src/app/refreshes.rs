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
pub(super) struct Refreshes {
    sessions: Refresh,
    input_history: Refresh,
}

pub(super) enum FinishedRefresh {
    None,
    Repeat(RuntimeRequest),
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
        true
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
        FinishedRefresh::None
    }
}

impl super::App {
    pub(super) fn continue_refresh(&mut self, finished: FinishedRefresh) {
        match finished {
            FinishedRefresh::None => {}
            FinishedRefresh::Repeat(request) => {
                self.request(request);
            }
        }
    }
}

#[cfg(test)]
mod tests;
