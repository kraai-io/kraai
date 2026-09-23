use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use color_eyre::eyre::{Result, eyre};
use kraai_types::{ModelId, ProviderId, TokenUsage};
use tokio::time::Instant;

use crate::{ProviderManager, ProviderRequest};

mod feedback;
mod prefix;
use feedback::{Feedback, input_tokens};
use prefix::{Prefix, fingerprints};

#[derive(Clone, Copy, Debug)]
pub struct CacheWarmingPolicy {
    pub min_prefix_bytes: usize,
    pub min_growth_tokens: usize,
    pub min_requests_between_warmups: usize,
    pub max_requests_between_warmups: usize,
    pub refresh_after: Duration,
    pub timeout: Duration,
}

impl Default for CacheWarmingPolicy {
    fn default() -> Self {
        Self {
            min_prefix_bytes: 8192,
            min_growth_tokens: 1024,
            min_requests_between_warmups: 2,
            max_requests_between_warmups: 5,
            refresh_after: Duration::from_secs(300),
            timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct CacheWarming {
    sessions: Arc<Mutex<HashMap<CacheKey, SharedState>>>,
}

type CacheKey = (ProviderId, ModelId, String);
type SharedState = Arc<Mutex<State>>;

#[derive(Default)]
struct State {
    completed: Option<CompletedWarmup>,
    feedback: Feedback,
    last_attempt_succeeded: bool,
    last_attempt: Option<Instant>,
    last_used: Option<Instant>,
    requests: usize,
    in_flight: bool,
}

struct CompletedWarmup {
    prefix: Prefix,
    input_tokens: usize,
}

pub struct CacheWarmup {
    pub request: ProviderRequest,
    pub timeout: Duration,
    prefix: Option<Prefix>,
    state: Arc<Mutex<State>>,
}

impl CacheWarmup {
    pub fn complete(mut self, usage: &TokenUsage) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| eyre!("Cache warming state poisoned: {error}"))?;
        state.feedback.observe_warmup(usage);
        state.completed = self.prefix.take().map(|prefix| CompletedWarmup {
            prefix,
            input_tokens: input_tokens(usage),
        });
        state.last_attempt_succeeded = true;
        drop(state);
        Ok(())
    }
}

impl Drop for CacheWarmup {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.in_flight = false;
        }
    }
}

impl ProviderManager {
    pub fn prepare_cache_warmup(
        &self,
        provider_id: &ProviderId,
        model_id: &ModelId,
        session_id: &str,
        request: &ProviderRequest,
    ) -> Result<Option<CacheWarmup>> {
        let Some(policy) = self
            .get_provider(provider_id)
            .and_then(|provider| provider.cache_warming_policy(model_id))
        else {
            return Ok(None);
        };
        if policy.min_requests_between_warmups == 0
            || policy.max_requests_between_warmups < policy.min_requests_between_warmups
        {
            return Err(eyre!("Invalid cache warming interval bounds"));
        }
        let Some(boundary) = request.cacheable_messages else {
            return Ok(None);
        };
        if boundary == 0 || boundary >= request.messages.len() {
            return Err(eyre!("Invalid cacheable message boundary"));
        }
        let messages = request
            .messages
            .get(..boundary)
            .ok_or_else(|| eyre!("Invalid cacheable message boundary"))?;
        let prefix = Prefix::new(messages, &request.script_tool)?;
        if prefix.bytes < policy.min_prefix_bytes {
            return Ok(None);
        }
        let now = Instant::now();
        let state = self
            .cache_warming
            .state(provider_id, model_id, session_id)?;
        let suffix = fingerprints(
            request
                .messages
                .get(boundary..)
                .ok_or_else(|| eyre!("Invalid cacheable message boundary"))?,
        )?
        .0;
        {
            let mut entry = state
                .lock()
                .map_err(|error| eyre!("Cache warming state poisoned: {error}"))?;
            entry.feedback.synchronize(&prefix, &suffix);
            if entry.feedback.has_no_discount() {
                return Ok(None);
            }
            entry.requests = entry.requests.saturating_add(1);
            if entry.in_flight {
                return Ok(None);
            }
            let expired = entry
                .last_attempt
                .is_none_or(|last| now.duration_since(last) >= policy.refresh_after);
            let changed = entry
                .completed
                .as_ref()
                .is_none_or(|old| !prefix.extends(&old.prefix));
            let grown = entry.completed.as_ref().is_some_and(|old| {
                prefix != old.prefix
                    && entry
                        .feedback
                        .predicted_prefix(&prefix)
                        .is_none_or(|tokens| {
                            tokens - old.input_tokens as f64 >= policy.min_growth_tokens as f64
                        })
            });
            let interval = if entry.last_attempt_succeeded {
                entry.feedback.interval(policy, &prefix)
            } else {
                policy.max_requests_between_warmups
            };
            if !expired && (entry.requests < interval || !changed && !grown) {
                return Ok(None);
            }
            tracing::debug!(
                interval,
                requests = entry.requests,
                "Scheduling cache warm-up"
            );
            entry.last_attempt_succeeded = false;
            entry.in_flight = true;
            entry.requests = 0;
            entry.last_attempt = Some(now);
        }
        Ok(Some(CacheWarmup {
            request: ProviderRequest {
                messages: messages.to_vec(),
                script_tool: request.script_tool.clone(),
                cacheable_messages: None,
            },
            timeout: policy.timeout,
            prefix: Some(prefix),
            state,
        }))
    }
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible test fixtures use direct assertions"
)]
mod tests;

impl CacheWarming {
    fn state(
        &self,
        provider_id: &ProviderId,
        model_id: &ModelId,
        session_id: &str,
    ) -> Result<SharedState> {
        let now = Instant::now();
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|error| eyre!("Cache warming sessions poisoned: {error}"))?;
        sessions.retain(|_, state| {
            Arc::strong_count(state) > 1
                || state.lock().is_ok_and(|state| {
                    state.in_flight
                        || state.last_used.is_some_and(|time| {
                            now.duration_since(time) < Duration::from_secs(3600)
                        })
                })
        });
        let state = sessions
            .entry((provider_id.clone(), model_id.clone(), session_id.to_owned()))
            .or_default()
            .clone();
        drop(sessions);
        state
            .lock()
            .map_err(|error| eyre!("Cache warming state poisoned: {error}"))?
            .last_used = Some(now);
        Ok(state)
    }
}

pub(crate) struct CacheUsageObserver {
    prefix: Prefix,
    suffix: Vec<u64>,
    state: SharedState,
}

impl CacheUsageObserver {
    pub(crate) fn observe(&self, usage: &TokenUsage) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| eyre!("Cache warming state poisoned: {error}"))?;
        let State {
            feedback,
            completed,
            ..
        } = &mut *state;
        feedback.observe(
            self.prefix.clone(),
            self.suffix.clone(),
            usage,
            completed.as_ref(),
        );
        drop(state);
        Ok(())
    }
}

impl ProviderManager {
    pub(crate) fn cache_usage_observer(
        &self,
        provider_id: &ProviderId,
        model_id: &ModelId,
        request: &ProviderRequest,
        context: &crate::ProviderRequestContext,
    ) -> Result<Option<CacheUsageObserver>> {
        let Some(session_id) = context.prompt_cache_key() else {
            return Ok(None);
        };
        let Some(boundary) = request.cacheable_messages else {
            return Ok(None);
        };
        if self
            .get_provider(provider_id)
            .and_then(|provider| provider.cache_warming_policy(model_id))
            .is_none()
        {
            return Ok(None);
        }
        let prefix = Prefix::new(
            request
                .messages
                .get(..boundary)
                .ok_or_else(|| eyre!("Invalid cacheable message boundary"))?,
            &request.script_tool,
        )?;
        let suffix = fingerprints(
            request
                .messages
                .get(boundary..)
                .ok_or_else(|| eyre!("Invalid cacheable message boundary"))?,
        )?
        .0;
        Ok(Some(CacheUsageObserver {
            prefix,
            suffix,
            state: self
                .cache_warming
                .state(provider_id, model_id, session_id)?,
        }))
    }
}
