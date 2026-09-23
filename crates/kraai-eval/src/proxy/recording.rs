use std::sync::atomic::Ordering;
use std::time::Instant;

use color_eyre::eyre::{Result, eyre};

use super::{
    DownstreamDelivery, ForwardOutcome, ParsedRequest, ProxyState, record_request_metrics,
    record_usage_metrics, tracing_fallback, write_event,
};

pub(super) struct RequestRecord<'a> {
    state: &'a ProxyState,
    request: &'a ParsedRequest,
    request_id: String,
    started: Instant,
    finished: bool,
    pub(super) outcome: ForwardOutcome,
}

impl<'a> RequestRecord<'a> {
    pub(super) fn new(state: &'a ProxyState, request: &'a ParsedRequest, started: Instant) -> Self {
        state.started_requests.fetch_add(1, Ordering::Relaxed);
        Self {
            state,
            request,
            request_id: ulid::Ulid::generate().to_string(),
            started,
            finished: false,
            outcome: ForwardOutcome::pending(),
        }
    }

    pub(super) fn finish(&mut self, result: Result<()>) -> Result<()> {
        self.finished = true;
        if let Err(error) = &result {
            self.outcome.error = Some(format!("{error:#}"));
            if self.outcome.delivery != DownstreamDelivery::ClientDisconnected {
                self.outcome.delivery = DownstreamDelivery::Incomplete;
            }
        } else {
            self.outcome.stage = "complete";
        }
        self.outcome.usage = record_usage_metrics(self.state, &self.outcome.body)?;
        let elapsed = self.started.elapsed();
        record_request_metrics(self.state, &self.outcome, elapsed)?;
        write_event(
            self.state,
            self.request,
            &self.request_id,
            &self.outcome,
            elapsed,
        )?;
        result
    }
}

impl Drop for RequestRecord<'_> {
    fn drop(&mut self) {
        if !self.finished
            && let Err(error) = self.finish(Err(eyre!(
                "proxy request cancelled before forwarding completed"
            )))
        {
            tracing_fallback(&format!("request {}: {error:#}", self.request_id));
        }
    }
}
