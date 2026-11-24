use std::collections::{BTreeMap, BTreeSet};

use kraai_types::{CostSummary, Message, MessageId, RequestUsage, SessionId};

use crate::app::types::{ExitUsageTotals, UsageModelKey};

pub(in crate::app) struct Usage {
    pub(in crate::app) session_cost: CostSummary,
    pub(in crate::app) session_requests: BTreeMap<SessionId, BTreeMap<MessageId, RequestUsage>>,
    pub(in crate::app) launch_requests: BTreeMap<MessageId, RequestUsage>,
    pub(in crate::app) launched_at: u64,
    pub(in crate::app) cost_recovery_list_pending: bool,
    pub(in crate::app) cost_recovery_sessions: BTreeSet<SessionId>,
    pub(in crate::app) exit_totals: ExitUsageTotals,
}

impl Default for Usage {
    fn default() -> Self {
        Self {
            session_cost: CostSummary::default(),
            session_requests: BTreeMap::new(),
            launch_requests: BTreeMap::new(),
            launched_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            cost_recovery_list_pending: false,
            cost_recovery_sessions: BTreeSet::new(),
            exit_totals: ExitUsageTotals::default(),
        }
    }
}

impl Usage {
    pub(in crate::app) fn mark_completed(&mut self, message_id: MessageId) {
        self.exit_totals.completed_message_ids.insert(message_id);
    }

    pub(in crate::app) fn accumulate_completed(&mut self, history: &BTreeMap<MessageId, Message>) {
        for message in history.values() {
            if !self.exit_totals.completed_message_ids.contains(&message.id) {
                continue;
            }
            let Some(generation) = message.generation.as_ref() else {
                continue;
            };
            let Some(usage) = generation.usage.as_ref() else {
                continue;
            };
            if !self
                .exit_totals
                .counted_message_ids
                .insert(message.id.clone())
            {
                continue;
            }

            let model_usage = self
                .exit_totals
                .usage_by_model
                .entry(UsageModelKey {
                    provider_id: generation.provider_id.to_string(),
                    model_id: generation.model_id.to_string(),
                })
                .or_default();
            model_usage.total_tokens = model_usage.total_tokens.saturating_add(usage.total_tokens);
            model_usage.input_tokens = model_usage.input_tokens.saturating_add(usage.input_tokens);
            model_usage.output_tokens = model_usage
                .output_tokens
                .saturating_add(usage.output_tokens);
            model_usage.reasoning_tokens = model_usage
                .reasoning_tokens
                .saturating_add(usage.reasoning_tokens);
            model_usage.cache_write_tokens = model_usage
                .cache_write_tokens
                .saturating_add(usage.cache_write_tokens);
            model_usage.cache_read_tokens = model_usage
                .cache_read_tokens
                .saturating_add(usage.cache_read_tokens);
        }
    }
}
