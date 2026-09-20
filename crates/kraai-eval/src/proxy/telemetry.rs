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

#[derive(Serialize)]
struct ProxyEvent<'a> {
    timestamp_ms: u128,
    method: &'a str,
    path: &'a str,
    status: u16,
    delivery: DownstreamDelivery,
    duration_ms: u128,
    usage: &'a Option<UsageMetrics>,
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
        method: &request.method,
        path: &request.path,
        status: outcome.status,
        delivery: outcome.delivery,
        duration_ms: duration.as_millis(),
        usage: &outcome.usage,
        routing_hint_derived: derived_hint.is_some(),
        model: field("model"),
        reasoning_effort: body
            .as_ref()
            .and_then(|value| value.pointer("/reasoning/effort"))
            .and_then(serde_json::Value::as_str),
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
mod tests {
    use color_eyre::eyre::ensure;

    use super::*;
    use crate::proxy::headers::{forward_request_headers, missing_routing_hint};

    #[test]
    fn shared_body_preserves_cache_hashes_and_malformed_body_fallback() -> Result<()> {
        for (body, cache_key) in [
            (r#"{"prompt_cache_key":"key"}"#, Some("key")),
            (r#"{"prompt_cache_key":""}"#, Some("")),
            (
                r#"{"prompt_cache_key":"old","prompt_cache_key":"new"}"#,
                Some("new"),
            ),
            (r#"{"prompt_cache_key":null}"#, None),
            (r#"{"prompt_cache_key":5}"#, None),
            (r#"{"prompt_cache_key":"key"} trailing"#, None),
            ("not json", None),
        ] {
            let request = ParsedRequest {
                method: String::from("POST"),
                target: String::from("/v1/responses"),
                path: String::from("/v1/responses"),
                headers: vec![(
                    String::from("session-id"),
                    HeaderValue::from_static("session"),
                )],
                body: body.as_bytes().to_vec().into(),
            };
            let parsed = serde_json::from_slice::<serde_json::Value>(&request.body).ok();
            let state = CacheState::from_request(&request, parsed.as_ref(), None);
            ensure!(state.session_id_sha256 == Some(fingerprint(b"session")));
            ensure!(
                state.prompt_cache_key_sha256 == cache_key.map(|key| fingerprint(key.as_bytes()))
            );
            ensure!(request.body == body.as_bytes());
        }
        Ok(())
    }

    #[test]
    fn routing_diagnostics_match_the_effective_subscription_request() -> Result<()> {
        let mut request = ParsedRequest {
            method: String::from("POST"),
            target: String::from("/backend-api/codex/responses"),
            path: String::from("/backend-api/codex/responses"),
            headers: Vec::new(),
            body: br#"{"model":"gpt-6-astra","service_tier":"priority"}"#
                .to_vec()
                .into(),
        };
        for supplied in [None, Some("model=supplied;tier=flex")] {
            if let Some(hint) = supplied {
                request.headers.push((
                    String::from("x-codex-routing-hint"),
                    HeaderValue::from_static(hint),
                ));
            }
            let derived_hint = missing_routing_hint(&request, true);
            ensure!(derived_hint.is_some() == supplied.is_none());
            let body = serde_json::from_slice::<serde_json::Value>(&request.body).ok();
            let logged = CacheState::from_request(&request, body.as_ref(), derived_hint.as_ref());
            let forwarded = forward_request_headers(
                reqwest::Client::new().post("https://upstream.invalid/responses"),
                &request,
                true,
            )
            .build()?;
            ensure!(
                logged.routing_hint_sha256
                    == forwarded
                        .headers()
                        .get("x-codex-routing-hint")
                        .map(|hint| fingerprint(hint.as_bytes()))
            );
            ensure!(logged.routing_hint_sha256.is_some());
        }
        Ok(())
    }
}
