use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::Result;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Serialize;

use super::headers::missing_routing_hint_from_body;
use super::{DownstreamDelivery, ForwardOutcome, ParsedRequest, ProxyState, UpstreamCredentials};
use crate::metrics::UsageMetrics;

#[derive(Default, Serialize)]
pub(super) struct CacheState {
    session_id_sha256: Option<String>,
    legacy_session_id_sha256: Option<String>,
    thread_id_sha256: Option<String>,
    routing_hint_sha256: Option<String>,
    turn_state_sha256: Option<String>,
    prompt_cache_key_sha256: Option<String>,
}

impl CacheState {
    fn from_request(
        request: &ParsedRequest,
        body: Option<&serde_json::Value>,
        derived_hint: Option<&HeaderValue>,
    ) -> Self {
        let header_hash = |name| {
            request
                .headers
                .iter()
                .find(|(header, _)| header == name)
                .map(|(_, value)| fingerprint(value.as_bytes()))
        };
        Self {
            session_id_sha256: header_hash("session-id"),
            legacy_session_id_sha256: header_hash("session_id"),
            thread_id_sha256: header_hash("thread-id"),
            routing_hint_sha256: header_hash("x-codex-routing-hint")
                .or_else(|| derived_hint.map(|hint| fingerprint(hint.as_bytes()))),
            turn_state_sha256: header_hash("x-codex-turn-state"),
            prompt_cache_key_sha256: body
                .and_then(|value| value.get("prompt_cache_key"))
                .and_then(serde_json::Value::as_str)
                .map(|value| fingerprint(value.as_bytes())),
        }
    }

    pub(super) fn from_response(headers: &HeaderMap) -> Self {
        let header_hash = |name: &str| headers.get(name).map(|value| fingerprint(value.as_bytes()));
        Self {
            routing_hint_sha256: header_hash("x-codex-routing-hint"),
            turn_state_sha256: header_hash("x-codex-turn-state"),
            ..Self::default()
        }
    }
}

fn fingerprint(bytes: &[u8]) -> String {
    crate::cache::hash_chunks(&[bytes])
}

fn reasoning_effort(body: &serde_json::Value) -> Option<&str> {
    body.pointer("/reasoning/effort")
        .or_else(|| body.get("reasoning_effort"))
        .and_then(serde_json::Value::as_str)
}

#[derive(Serialize)]
struct ProxyEvent<'a> {
    timestamp_ms: u128,
    request_id: &'a str,
    upstream_request_id: &'a Option<String>,
    method: &'a str,
    path: &'a str,
    status: Option<u16>,
    delivery: DownstreamDelivery,
    error: &'a Option<String>,
    stage: &'a str,
    response_bytes: usize,
    duration_ms: u128,
    usage: &'a Option<UsageMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reported_cost: Option<kraai_types::Usd>,
    routing_hint_derived: bool,
    model: Option<&'a str>,
    reasoning_effort: Option<&'a str>,
    service_tier: Option<&'a str>,
    request_cache_state: CacheState,
    response_cache_state: &'a CacheState,
}

pub(super) fn write_event(
    state: &ProxyState,
    request: &ParsedRequest,
    request_id: &str,
    outcome: &ForwardOutcome,
    duration: Duration,
) -> Result<()> {
    let timestamp_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let body = serde_json::from_slice::<serde_json::Value>(&request.body).ok();
    let field = |name| {
        body.as_ref()
            .and_then(|value| value.get(name))
            .and_then(serde_json::Value::as_str)
    };
    let derived_hint = missing_routing_hint_from_body(
        request,
        matches!(state.credentials, UpstreamCredentials::Codex { .. }),
        body.as_ref(),
    );
    let event = ProxyEvent {
        timestamp_ms,
        request_id,
        upstream_request_id: &outcome.upstream_request_id,
        method: &request.method,
        path: &request.path,
        status: outcome.status,
        delivery: outcome.delivery,
        error: &outcome.error,
        stage: outcome.stage,
        response_bytes: outcome.body.len(),
        duration_ms: duration.as_millis(),
        usage: &outcome.usage,
        reported_cost: outcome.reported_cost,
        routing_hint_derived: derived_hint.is_some(),
        model: field("model"),
        reasoning_effort: body.as_ref().and_then(reasoning_effort),
        service_tier: field("service_tier"),
        request_cache_state: CacheState::from_request(
            request,
            body.as_ref(),
            derived_hint.as_ref(),
        ),
        response_cache_state: &outcome.response_cache_state,
    };
    let mut log = state
        .log
        .lock()
        .map_err(|error| color_eyre::eyre::eyre!("proxy log mutex poisoned: {error}"))?;
    serde_json::to_writer(&mut *log, &event)?;
    log.write_all(b"\n")?;
    log.flush()?;
    drop(log);
    Ok(())
}

#[cfg(test)]
mod tests;
