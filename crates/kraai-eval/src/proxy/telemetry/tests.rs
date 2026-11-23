use super::*;
use crate::proxy::headers::{forward_request_headers, missing_routing_hint};

#[test]
fn reasoning_effort_reads_responses_and_chat_completions_bodies() {
    for (body, expected) in [
        (
            serde_json::json!({"reasoning": {"effort": "high"}}),
            Some("high"),
        ),
        (serde_json::json!({"reasoning_effort": "low"}), Some("low")),
        (serde_json::json!({"model": "model"}), None),
    ] {
        assert_eq!(reasoning_effort(&body), expected);
    }
}

#[test]
fn cache_hashes_use_the_effective_key_and_tolerate_malformed_bodies() {
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
        assert!(state.session_id_sha256 == Some(fingerprint(b"session")));
        assert!(state.prompt_cache_key_sha256 == cache_key.map(|key| fingerprint(key.as_bytes())));
    }
}

#[test]
fn routing_diagnostics_match_the_effective_subscription_request() {
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
        assert!(derived_hint.is_some() == supplied.is_none());
        let body = serde_json::from_slice::<serde_json::Value>(&request.body).ok();
        let logged = CacheState::from_request(&request, body.as_ref(), derived_hint.as_ref());
        let forwarded = forward_request_headers(
            reqwest::Client::new().post("https://upstream.invalid/responses"),
            &request,
            true,
        )
        .build()
        .unwrap();
        assert!(
            logged.routing_hint_sha256
                == forwarded
                    .headers()
                    .get("x-codex-routing-hint")
                    .map(|hint| fingerprint(hint.as_bytes()))
        );
        assert!(logged.routing_hint_sha256.is_some());
    }
}
