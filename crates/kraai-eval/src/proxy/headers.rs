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
    if !codex_subscription
        || request.method != "POST"
        || !matches!(
            request.path.as_str(),
            "/codex/responses" | "/backend-api/codex/responses"
        )
        || request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("x-codex-routing-hint"))
    {
        return None;
    }
    let body: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
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
mod tests {
    use color_eyre::eyre::{Result, ensure};
    use reqwest::{Client, Method};

    use super::*;

    fn request(body: &[u8]) -> ParsedRequest {
        ParsedRequest {
            method: String::from("POST"),
            target: String::from("/backend-api/codex/responses"),
            path: String::from("/backend-api/codex/responses"),
            headers: Vec::new(),
            body: body.to_vec(),
        }
    }

    fn forwarded(request: &ParsedRequest, codex_subscription: bool) -> Result<reqwest::Request> {
        let builder = Client::new().request(Method::POST, "https://upstream.invalid/responses");
        Ok(
            forward_request_headers(builder, request, codex_subscription)
                .body(request.body.clone())
                .build()?,
        )
    }

    #[test]
    fn preserves_both_session_headers_and_filters_credentials() -> Result<()> {
        let mut request = request(br#"{"model":"gpt-6-astra"}"#);
        request.headers = [
            ("session_id", "kraai-session"),
            ("Session-Id", "codex-session"),
            ("thread-id", "codex-thread"),
            ("x-codex-turn-state", "turn-state"),
            ("authorization", "Bearer secret"),
            ("chatgpt-account-id", "account-secret"),
            ("cookie", "cookie-secret"),
            ("host", "wrong-host.invalid"),
            ("connection", "keep-alive"),
            ("proxy-authorization", "proxy-secret"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), HeaderValue::from_static(value)))
        .collect();
        let forwarded = forwarded(&request, true)?;
        for (name, value) in [
            ("session_id", "kraai-session"),
            ("session-id", "codex-session"),
            ("thread-id", "codex-thread"),
            ("x-codex-turn-state", "turn-state"),
        ] {
            ensure!(
                forwarded
                    .headers()
                    .get(name)
                    .is_some_and(|actual| actual == value)
            );
        }
        for name in [
            "authorization",
            "chatgpt-account-id",
            "cookie",
            "host",
            "connection",
            "proxy-authorization",
        ] {
            ensure!(!forwarded.headers().contains_key(name));
        }
        Ok(())
    }

    #[test]
    fn derives_subscription_routing_hint_without_changing_body() -> Result<()> {
        for (body, expected) in [
            (r#"{"model":"gpt-6-astra"}"#, "model=gpt-6-astra"),
            (
                r#"{"model":"gpt-6-astra","service_tier":"priority","prompt_cache_key":"cache-key"}"#,
                "model=gpt-6-astra;tier=priority",
            ),
            (
                r#"{"model":"gpt-6-astra","service_tier":null}"#,
                "model=gpt-6-astra",
            ),
        ] {
            let request = request(body.as_bytes());
            let forwarded = forwarded(&request, true)?;
            ensure!(
                forwarded
                    .headers()
                    .get("x-codex-routing-hint")
                    .is_some_and(|hint| hint == expected)
            );
            ensure!(forwarded.body().and_then(reqwest::Body::as_bytes) == Some(body.as_bytes()));
        }
        Ok(())
    }

    #[test]
    fn routing_hint_derivation_is_scoped_and_preserves_supplied_hints() -> Result<()> {
        let mut request = request(br#"{"model":"gpt-6-astra"}"#);
        ensure!(missing_routing_hint(&request, false).is_none());
        request.method = String::from("GET");
        ensure!(missing_routing_hint(&request, true).is_none());
        request.method = String::from("POST");
        for path in [
            "/v1/responses",
            "/backend-api/models",
            "/other/codex/responses",
        ] {
            request.path = path.to_owned();
            ensure!(missing_routing_hint(&request, true).is_none());
        }
        request.path = String::from("/codex/responses");
        ensure!(missing_routing_hint(&request, true).is_some());
        request.headers.push((
            String::from("X-Codex-Routing-Hint"),
            HeaderValue::from_static("model=custom;tier=flex"),
        ));
        let forwarded = forwarded(&request, true)?;
        let hints = forwarded.headers().get_all("x-codex-routing-hint");
        ensure!(hints.iter().count() == 1);
        ensure!(
            hints
                .iter()
                .next()
                .is_some_and(|hint| hint == "model=custom;tier=flex")
        );
        Ok(())
    }

    #[test]
    fn invalid_routing_fields_do_not_produce_headers() {
        for body in [
            "invalid json",
            r#"{}"#,
            r#"{"model":1}"#,
            r#"{"model":""}"#,
            r#"{"model":"   "}"#,
            r#"{"model":"model\r\nx-injected: true"}"#,
            r#"{"model":"model","service_tier":"priority\n"}"#,
            r#"{"model":"model","service_tier":1}"#,
            r#"{"model":"model","service_tier":""}"#,
        ] {
            assert!(missing_routing_hint(&request(body.as_bytes()), true).is_none());
        }
    }

    #[test]
    fn response_preserves_routing_bytes_and_controls_framing() -> Result<()> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
        headers.insert(
            "x-codex-turn-state",
            HeaderValue::from_bytes(b"opaque-\xff-state")?,
        );
        headers.insert("x-request-id", HeaderValue::from_static("request-123"));
        headers.insert("openai-model", HeaderValue::from_static("gpt-6-astra"));
        for name in [
            "authorization",
            "chatgpt-account-id",
            "set-cookie",
            "proxy-authenticate",
            "content-length",
            "transfer-encoding",
            "connection",
        ] {
            headers.insert(name, HeaderValue::from_static("not-forwarded"));
        }
        let head = response_head(StatusCode::OK, &headers);
        let expected = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nx-codex-turn-state: opaque-\xff-state\r\nx-request-id: request-123\r\nopenai-model: gpt-6-astra\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
        ensure!(head == expected);
        ensure!(
            response_head(StatusCode::BAD_GATEWAY, &HeaderMap::new())
                == b"HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/octet-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        );
        Ok(())
    }
}
