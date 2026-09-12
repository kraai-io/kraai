use super::*;

const ROUTING_HINT: &str = "\u{a0}route=v1.AbC_123-xy==;zone=eu-1\u{a0}";
const TURN_STATE: &[u8] = b"\xc2\xa0turn.v1:AbC+\xff/==\xc2\xa0";
const SESSION_ID: &str = "stable-session-id";
const LEGACY_SESSION_ID: &str = "stable-legacy-session-id";
const THREAD_ID: &str = "stable-thread-id";
const REQUEST_BODY: &str = r#"{
  "model": "test-model", "prompt_cache_key": "cache-case-sensitive",
  "input": "private-request-body", "stream": true
}"#;

#[tokio::test]
async fn routing_state_round_trips_without_credentials_and_counts_final_usage_once() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-proxy-routing-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root)?;
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        exercise_routing_round_trip(root.join("proxy.events.jsonl")),
    )
    .await;
    fs::remove_dir_all(root)?;
    result??;
    Ok(())
}

async fn exercise_routing_round_trip(log_path: PathBuf) -> Result<()> {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let upstream_address = upstream.local_addr()?;
    let upstream_task = tokio::spawn(async move {
        for index in 0..2 {
            let (mut stream, _) = upstream.accept().await?;
            let request = read_request(&mut stream).await?;
            ensure!(request.target == "/v1/responses", "request path changed");
            ensure!(
                request.body == REQUEST_BODY.as_bytes(),
                "request body or prompt_cache_key changed"
            );
            let authorization = request
                .headers
                .iter()
                .filter(|(name, _)| name == "authorization")
                .map(|(_, value)| value.as_bytes())
                .collect::<Vec<_>>();
            ensure!(
                authorization == [b"Bearer upstream-secret".as_slice()],
                "upstream credential was not substituted exactly once"
            );
            for (name, expected) in [
                ("session-id", SESSION_ID),
                ("session_id", LEGACY_SESSION_ID),
                ("thread-id", THREAD_ID),
            ] {
                let values = request
                    .headers
                    .iter()
                    .filter(|(header, _)| header == name)
                    .map(|(_, value)| value.as_bytes())
                    .collect::<Vec<_>>();
                ensure!(
                    values == [expected.as_bytes()],
                    "stable {name} changed or was dropped"
                );
            }
            for name in [
                "chatgpt-account-id",
                "cookie",
                "proxy-authorization",
                "x-api-key",
            ] {
                ensure!(
                    !request.headers.iter().any(|(header, _)| header == name),
                    "untrusted {name} reached the upstream"
                );
            }
            for (name, expected) in [
                ("x-codex-routing-hint", ROUTING_HINT.as_bytes()),
                ("x-codex-turn-state", TURN_STATE),
            ] {
                let value = request
                    .headers
                    .iter()
                    .find(|(header, _)| header == name)
                    .map(|(_, value)| value.as_bytes());
                if index == 0 {
                    ensure!(value.is_none(), "proxy invented initial {name}");
                } else {
                    ensure!(
                        value == Some(expected),
                        "returned {name} was dropped or changed on the next request"
                    );
                }
            }
            let body = response_events(index);
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\nX-Codex-Routing-Hint: {ROUTING_HINT}\r\nX-Codex-Turn-State: ",
                body.len()
            ).into_bytes();
            response.extend_from_slice(TURN_STATE);
            response.extend_from_slice(format!(
                "\r\nSet-Cookie: upstream-cookie=secret\r\nAuthorization: Bearer response-secret\r\nProxy-Authorization: Bearer response-proxy-secret\r\nX-Private-Secret: private-response-header\r\n\r\n{body}"
            ).as_bytes());
            stream.write_all(&response).await?;
            stream.shutdown().await?;
        }
        Ok::<_, color_eyre::Report>(())
    });

    let proxy = ModelProxy::start(ProxyServerConfig {
        upstream: format!("http://{upstream_address}"),
        credentials: UpstreamCredentials::OpenAiApiKey {
            credential: String::from("upstream-secret"),
            credential_env: String::from("TEST_API_KEY"),
        },
        allowed_paths: BTreeSet::from([String::from("/v1/responses")]),
        kind: String::from("openai"),
        base_path: String::from("/v1"),
        log_path: log_path.clone(),
        max_requests: 2,
    })?;
    let client = Client::builder().timeout(Duration::from_secs(5)).build()?;
    let mut routing = None;
    let mut turn_state = None;
    let proxy_token = proxy.token.clone();
    for index in 0..2 {
        let mut request = client
            .post(format!("{}/responses", proxy.base_url()))
            .bearer_auth(&proxy.token)
            .header("content-type", "application/json")
            .header("session-id", SESSION_ID)
            .header("session_id", LEGACY_SESSION_ID)
            .header("thread-id", THREAD_ID)
            .header("chatgpt-account-id", "spoofed-account")
            .header("cookie", "spoofed-cookie=secret")
            .header("proxy-authorization", "Bearer spoofed-proxy-secret")
            .header("x-api-key", "spoofed-api-secret")
            .body(REQUEST_BODY);
        if let Some(value) = routing.take() {
            request = request.header("x-codex-routing-hint", value);
        }
        if let Some(value) = turn_state.take() {
            request = request.header("x-codex-turn-state", value);
        }
        let response = request.send().await?;
        ensure!(response.status() == 200, "forwarded request failed");
        routing = response.headers().get("x-codex-routing-hint").cloned();
        turn_state = response.headers().get("x-codex-turn-state").cloned();
        ensure!(
            routing.as_ref().map(reqwest::header::HeaderValue::as_bytes)
                == Some(ROUTING_HINT.as_bytes()),
            "upstream routing hint did not reach the client unchanged"
        );
        ensure!(
            turn_state
                .as_ref()
                .map(reqwest::header::HeaderValue::as_bytes)
                == Some(TURN_STATE),
            "upstream turn state did not reach the client unchanged"
        );
        for name in [
            "set-cookie",
            "authorization",
            "proxy-authorization",
            "x-private-secret",
        ] {
            ensure!(
                !response.headers().contains_key(name),
                "upstream {name} leaked to the client"
            );
        }
        ensure!(
            response.text().await? == response_events(index),
            "response events changed"
        );
    }
    upstream_task.await??;
    let metrics = proxy.finish()?;
    ensure!(metrics.requests == 2 && metrics.successful_requests == 2);
    ensure!(metrics.failed_requests == 0 && metrics.client_disconnects == 0);
    ensure!(
        metrics.usage
            == UsageMetrics {
                total_tokens: 11270,
                input_tokens: 2808,
                output_tokens: 225,
                reasoning_tokens: 45,
                cache_read_tokens: 8192,
            },
        "stream usage was not counted once per final response"
    );
    let log = fs::read_to_string(log_path)?;
    for secret in [
        ROUTING_HINT,
        "turn.v1:AbC+",
        SESSION_ID,
        LEGACY_SESSION_ID,
        THREAD_ID,
        "upstream-secret",
        "response-secret",
        "spoofed-account",
        "spoofed-cookie",
        "spoofed-proxy-secret",
        "spoofed-api-secret",
        "private-request-body",
        "cache-case-sensitive",
        &proxy_token,
    ] {
        ensure!(!log.contains(secret), "request metadata leaked into logs");
    }
    let events = log
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(events.len() == 2, "requests were not logged exactly once");
    for (index, event) in events.iter().enumerate() {
        let expected = if index == 0 {
            UsageMetrics {
                total_tokens: 5120,
                input_tokens: 904,
                output_tokens: 100,
                reasoning_tokens: 20,
                cache_read_tokens: 4096,
            }
        } else {
            UsageMetrics {
                total_tokens: 6150,
                input_tokens: 1904,
                output_tokens: 125,
                reasoning_tokens: 25,
                cache_read_tokens: 4096,
            }
        };
        ensure!(
            event.get("usage") == Some(&serde_json::to_value(expected)?),
            "request log lacks the final usage breakdown"
        );
        for (field, value) in [
            ("session_id_sha256", Some(SESSION_ID.as_bytes())),
            (
                "legacy_session_id_sha256",
                Some(LEGACY_SESSION_ID.as_bytes()),
            ),
            ("thread_id_sha256", Some(THREAD_ID.as_bytes())),
            (
                "prompt_cache_key_sha256",
                Some(b"cache-case-sensitive".as_slice()),
            ),
            (
                "routing_hint_sha256",
                (index == 1).then_some(ROUTING_HINT.as_bytes()),
            ),
            ("turn_state_sha256", (index == 1).then_some(TURN_STATE)),
        ] {
            assert_state_digest(event, "request_cache_state", field, value)?;
        }
        for (field, value) in [
            ("session_id_sha256", None),
            ("legacy_session_id_sha256", None),
            ("thread_id_sha256", None),
            ("prompt_cache_key_sha256", None),
            ("routing_hint_sha256", Some(ROUTING_HINT.as_bytes())),
            ("turn_state_sha256", Some(TURN_STATE)),
        ] {
            assert_state_digest(event, "response_cache_state", field, value)?;
        }
    }
    Ok(())
}

fn assert_state_digest(
    event: &serde_json::Value,
    state: &str,
    field: &str,
    value: Option<&[u8]>,
) -> Result<()> {
    let state = event
        .get(state)
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| color_eyre::eyre::eyre!("request log lacks {state}"))?;
    let expected = value.map(|value| crate::cache::hash_chunks(&[value.to_vec()]));
    ensure!(
        state.get(field).and_then(serde_json::Value::as_str) == expected.as_deref(),
        "cache state {field} does not match the forwarded value"
    );
    Ok(())
}

fn response_events(index: usize) -> String {
    let (input, output, reasoning) = if index == 0 {
        (5000, 120, 20)
    } else {
        (6000, 150, 25)
    };
    let final_event = serde_json::json!({
        "type": "response.completed",
        "response": {
            "usage": {
                "input_tokens": input,
                "input_tokens_details": { "cached_tokens": 4096 },
                "output_tokens": output,
                "output_tokens_details": { "reasoning_tokens": reasoning },
                "total_tokens": input + output
            }
        }
    });
    format!(
        "data: {{\"type\":\"response.in_progress\",\"response\":{{\"usage\":{{\"input_tokens\":4000,\"output_tokens\":100,\"total_tokens\":4100}}}}}}\n\ndata: {final_event}\n\ndata: {final_event}\n\ndata: [DONE]\n\n"
    )
}
