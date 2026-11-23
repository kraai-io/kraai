use reqwest::{Client, Method};

use super::*;

fn request(body: &[u8]) -> ParsedRequest {
    ParsedRequest {
        method: String::from("POST"),
        target: String::from("/backend-api/codex/responses"),
        path: String::from("/backend-api/codex/responses"),
        headers: Vec::new(),
        body: body.to_vec().into(),
    }
}

fn forwarded(request: &ParsedRequest, codex_subscription: bool) -> reqwest::Request {
    let builder = Client::new().request(Method::POST, "https://upstream.invalid/responses");
    forward_request_headers(builder, request, codex_subscription)
        .body(request.body.clone())
        .build()
        .unwrap()
}

#[test]
fn preserves_both_session_headers_and_filters_credentials() {
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
    let forwarded = forwarded(&request, true);
    for (name, value) in [
        ("session_id", "kraai-session"),
        ("session-id", "codex-session"),
        ("thread-id", "codex-thread"),
        ("x-codex-turn-state", "turn-state"),
    ] {
        assert!(
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
        assert!(!forwarded.headers().contains_key(name));
    }
}

#[test]
fn derives_subscription_routing_hint() {
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
        let forwarded = forwarded(&request, true);
        assert!(
            forwarded
                .headers()
                .get("x-codex-routing-hint")
                .is_some_and(|hint| hint == expected)
        );
    }
}

#[test]
fn routing_hint_derivation_is_scoped_and_preserves_supplied_hints() {
    let mut request = request(br#"{"model":"gpt-6-astra"}"#);
    assert!(missing_routing_hint(&request, false).is_none());
    request.method = String::from("GET");
    assert!(missing_routing_hint(&request, true).is_none());
    request.method = String::from("POST");
    for path in [
        "/v1/responses",
        "/backend-api/models",
        "/other/codex/responses",
    ] {
        request.path = path.to_owned();
        assert!(missing_routing_hint(&request, true).is_none());
    }
    request.path = String::from("/codex/responses");
    assert!(missing_routing_hint(&request, true).is_some());
    request.headers.push((
        String::from("X-Codex-Routing-Hint"),
        HeaderValue::from_static("model=custom;tier=flex"),
    ));
    let forwarded = forwarded(&request, true);
    let hints = forwarded.headers().get_all("x-codex-routing-hint");
    assert!(hints.iter().count() == 1);
    assert!(
        hints
            .iter()
            .next()
            .is_some_and(|hint| hint == "model=custom;tier=flex")
    );
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
        let request = request(body.as_bytes());
        let parsed = serde_json::from_slice::<serde_json::Value>(&request.body).ok();
        assert!(missing_routing_hint(&request, true).is_none());
        assert!(missing_routing_hint_from_body(&request, true, parsed.as_ref()).is_none());
    }
}

#[test]
fn parsed_routing_hints_preserve_field_selection_and_scope() {
    for body in [
        r#"{"model":"model"}"#,
        r#"{"model":"old","model":"model","service_tier":"priority"}"#,
        r#"{"model":"model","service_tier":null}"#,
        r#"{"model":"model","service_tier":" priority "}"#,
        r#"{"model":"模型"}"#,
        r#"{"model":"model","service_tier":false}"#,
        r#"{"model":"model"} trailing"#,
    ] {
        let mut request = request(body.as_bytes());
        let parsed = serde_json::from_slice::<serde_json::Value>(&request.body).ok();
        for codex in [false, true] {
            for method in ["GET", "POST"] {
                request.method = method.to_owned();
                for path in [
                    "/v1/responses",
                    "/codex/responses",
                    "/backend-api/codex/responses",
                ] {
                    request.path = path.to_owned();
                    for supplied in [false, true] {
                        request.headers.clear();
                        if supplied {
                            request.headers.push((
                                String::from("X-Codex-Routing-Hint"),
                                HeaderValue::from_static("model=supplied"),
                            ));
                        }
                        assert_eq!(
                            missing_routing_hint_from_body(&request, codex, parsed.as_ref()),
                            missing_routing_hint(&request, codex)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn response_preserves_routing_bytes_and_controls_framing() {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    headers.insert(
        "x-codex-turn-state",
        HeaderValue::from_bytes(b"opaque-\xff-state").unwrap(),
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
    assert!(head == expected);
    assert!(
        response_head(StatusCode::BAD_GATEWAY, &HeaderMap::new())
            == b"HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/octet-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    );
}
