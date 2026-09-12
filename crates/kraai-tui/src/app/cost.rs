use std::collections::BTreeMap;

use kraai_types::{CostSummary, MessageId, RequestUsage};

use super::{App, UsageModelKey};

impl App {
    pub(super) fn costs_incomplete(&self) -> bool {
        self.state.cost_recovery_list_pending || !self.state.cost_recovery_sessions.is_empty()
    }

    pub(super) fn update_costs(
        &mut self,
        session_id: &str,
        requests: BTreeMap<MessageId, RequestUsage>,
    ) {
        let session_requests = self
            .state
            .session_requests
            .entry(session_id.into())
            .or_default();
        for (id, request) in requests {
            let request = session_requests
                .entry(id.clone())
                .and_modify(|existing| {
                    existing.unpriced_attempts =
                        existing.unpriced_attempts.max(request.unpriced_attempts);
                    if request.usage.is_some()
                        && (existing
                            .usage
                            .as_ref()
                            .and_then(|usage| usage.cost.as_ref())
                            .is_none()
                            || request
                                .usage
                                .as_ref()
                                .and_then(|usage| usage.cost.as_ref())
                                .is_some())
                    {
                        existing.usage = request.usage.clone();
                    }
                    if existing.started_at == 0 {
                        existing.started_at = request.started_at;
                        existing.subscription = request.subscription;
                    }
                })
                .or_insert(request);
            if request.started_at >= self.state.launched_at {
                self.state.launch_requests.insert(id, request.clone());
            }
        }
        if self.state.current_session_id.as_deref() == Some(session_id) {
            self.state.session_cost = summarize_reported(session_requests.values());
        }
    }

    pub(super) fn restore_session_cost(&mut self) {
        self.state.session_cost = self
            .state
            .current_session_id
            .as_ref()
            .and_then(|id| self.state.session_requests.get(id))
            .map(|requests| summarize_reported(requests.values()))
            .unwrap_or_default();
    }

    pub(super) fn exit_cost_summary(&self) -> Vec<String> {
        if self.state.launch_requests.is_empty() && !self.costs_incomplete() {
            return Vec::new();
        }
        let mut models: BTreeMap<UsageModelKey, CostSummary> = BTreeMap::new();
        for request in self.state.launch_requests.values() {
            models
                .entry(UsageModelKey {
                    provider_id: request.provider_id.to_string(),
                    model_id: request.model_id.to_string(),
                })
                .or_default()
                .add(request);
        }
        let mut lines = vec![String::from("Cost since launch:")];
        for (model, summary) in models {
            lines.push(format!(
                "  {}/{}: {summary}",
                model.provider_id, model.model_id
            ));
        }
        let total = summarize(self.state.launch_requests.values());
        if self.costs_incomplete() {
            lines.push(format!(
                "  total: {total} (incomplete: runtime events missed)"
            ));
        } else {
            lines.push(format!("  total: {total}"));
        }
        lines
    }
}

fn summarize<'a>(requests: impl Iterator<Item = &'a RequestUsage>) -> CostSummary {
    let mut summary = CostSummary::default();
    for request in requests {
        summary.add(request);
    }
    summary
}

fn summarize_reported<'a>(requests: impl Iterator<Item = &'a RequestUsage>) -> CostSummary {
    let mut summary = CostSummary::default();
    for request in requests {
        if request.usage.is_some() {
            summary.add(request);
        } else {
            summary.unknown = summary
                .unknown
                .saturating_add(request.unpriced_attempts as usize);
        }
    }
    summary
}
