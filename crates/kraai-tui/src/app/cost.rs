use std::collections::BTreeMap;

use kraai_types::{CostSummary, MessageId, RequestUsage};

use super::{App, UsageModelKey};

impl App {
    pub(super) fn update_costs(
        &mut self,
        session_id: &str,
        requests: BTreeMap<MessageId, RequestUsage>,
    ) {
        if self.state.current_session_id.as_deref() == Some(session_id) {
            self.state.session_cost = summarize(requests.values());
        }
        for (id, request) in requests {
            if request.started_at >= self.state.launched_at {
                self.state.launch_requests.insert(id, request);
            }
        }
    }

    pub(super) fn exit_cost_summary(&self) -> Vec<String> {
        if self.state.launch_requests.is_empty() {
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
        lines.push(format!(
            "  total: {}",
            summarize(self.state.launch_requests.values())
        ));
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
