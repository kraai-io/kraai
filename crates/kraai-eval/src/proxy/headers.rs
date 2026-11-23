use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{RequestBuilder, StatusCode};

use super::ParsedRequest;

const REQUEST_HEADERS: &[&str] = &[
    "accept",
    "content-type",
    "openai-beta",
    "user-agent",
    "session_id",
    "session-id",
    "thread-id",
    "x-client-request-id",
    "x-codex-routing-hint",
    "x-codex-turn-state",
    "x-codex-turn-metadata",
    "x-codex-beta-features",
];

const RESPONSE_HEADERS: &[&str] = &[
    "x-codex-turn-state",
    "x-codex-routing-hint",
    "x-request-id",
    "openai-model",
    "x-reasoning-included",
    "x-models-etag",
    "retry-after",
];

pub(super) fn forward_request_headers(
    mut builder: RequestBuilder,
    request: &ParsedRequest,
    codex_subscription: bool,
) -> RequestBuilder {
    for (name, value) in &request.headers {
        if REQUEST_HEADERS
            .iter()
            .any(|allowed| name.eq_ignore_ascii_case(allowed))
        {
            builder = builder.header(name, value);
        }
    }
    if let Some(hint) = missing_routing_hint(request, codex_subscription) {
        builder = builder.header("x-codex-routing-hint", hint);
    }
    builder
}

pub(super) fn missing_routing_hint(
    request: &ParsedRequest,
    codex_subscription: bool,
) -> Option<HeaderValue> {
    if !needs_routing_hint(request, codex_subscription) {
        return None;
    }
    let body: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
    routing_hint_from_body(&body)
}

pub(super) fn missing_routing_hint_from_body(
    request: &ParsedRequest,
    codex_subscription: bool,
    body: Option<&serde_json::Value>,
) -> Option<HeaderValue> {
    if !needs_routing_hint(request, codex_subscription) {
        return None;
    }
    routing_hint_from_body(body?)
}

fn needs_routing_hint(request: &ParsedRequest, codex_subscription: bool) -> bool {
    codex_subscription
        && request.method == "POST"
        && matches!(
            request.path.as_str(),
            "/codex/responses" | "/backend-api/codex/responses"
        )
        && !request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("x-codex-routing-hint"))
}

fn routing_hint_from_body(body: &serde_json::Value) -> Option<HeaderValue> {
    let model = body.get("model")?.as_str()?;
    if model.trim().is_empty() {
        return None;
    }
    let hint = match body.get("service_tier") {
        None | Some(serde_json::Value::Null) => format!("model={model}"),
        Some(serde_json::Value::String(tier)) if !tier.trim().is_empty() => {
            format!("model={model};tier={tier}")
        }
        Some(_) => return None,
    };
    HeaderValue::from_str(&hint).ok()
}

pub(super) fn response_head(status: StatusCode, headers: &HeaderMap) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: ",
        status.as_u16(),
        status.canonical_reason().unwrap_or("Upstream Response")
    )
    .into_bytes();
    head.extend_from_slice(headers.get(CONTENT_TYPE).map_or(
        b"application/octet-stream".as_slice(),
        HeaderValue::as_bytes,
    ));
    head.extend_from_slice(b"\r\n");
    for name in RESPONSE_HEADERS {
        for value in headers.get_all(*name) {
            head.extend_from_slice(name.as_bytes());
            head.extend_from_slice(b": ");
            head.extend_from_slice(value.as_bytes());
            head.extend_from_slice(b"\r\n");
        }
    }
    head.extend_from_slice(b"Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
    head
}

#[cfg(test)]
mod tests;
