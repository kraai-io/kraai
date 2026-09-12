mod routing;

#[tokio::test]
async fn request_parser_preserves_header_octets_and_rejects_invalid_syntax() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let request = parse_raw_request(
            b"POST /v1/responses HTTP/1.1\r\nContent-Length: 2\r\nX-Codex-Turn-State:\t \xc2\xa0opaque-\xff\xc2\xa0 \t\r\n\r\n{}",
        )
        .await?;
        ensure!(
            request.headers.iter().any(|(name, value)| {
                name == "x-codex-turn-state"
                    && value.as_bytes() == b"\xc2\xa0opaque-\xff\xc2\xa0"
            })
        );
        ensure!(request.body == b"{}");
        for raw in [
            b"POST /v1/responses HTTP/1.1 extra\r\n\r\n".as_slice(),
            b"PO\xffST /v1/responses HTTP/1.1\r\n\r\n",
            b"POST /v1/responses HTTP/1.1\r\nBad Name: value\r\n\r\n",
            b"POST /v1/responses HTTP/1.1\r\nBad\xffName: value\r\n\r\n",
            b"POST /v1/responses HTTP/1.1\r\nX-State: invalid\x00value\r\n\r\n",
            b"POST /v1/responses HTTP/1.1\r\nX-State: \x0bvalue\r\n\r\n",
            b"POST /v1/responses HTTP/1.1\nX-State: value\r\n\r\n",
        ] {
            ensure!(parse_raw_request(raw).await.is_err(), "invalid HTTP was accepted");
        }
        Ok::<_, color_eyre::Report>(())
    })
    .await??;
    Ok(())
}

async fn parse_raw_request(raw: &[u8]) -> Result<ParsedRequest> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let address = listener.local_addr()?;
    let send = async {
        let mut client = TcpStream::connect(address).await?;
        client.write_all(raw).await?;
        client.shutdown().await?;
        Ok::<_, color_eyre::Report>(())
    };
    let receive = async {
        let (mut stream, _) = listener.accept().await?;
        read_request(&mut stream).await
    };
    let (sent, request) = tokio::join!(send, receive);
    sent?;
    request
}

use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use color_eyre::eyre::ensure;
use kraai_provider_openai_codex::OpenAiCodexAuthControllerOptions;

#[tokio::test]
async fn proxy_injects_real_credential_streams_and_rejects_unallowed_requests() -> Result<()> {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let upstream_address = upstream.local_addr()?;
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await?;
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                bail!("upstream client disconnected before headers");
            }
            request.extend_from_slice(chunk.get(..read).unwrap_or_default());
            if find_header_end(&request).is_some() {
                break;
            }
        }
        let request = String::from_utf8(request)?;
        ensure!(
            request.contains("authorization: Bearer real-secret")
                || request.contains("Authorization: Bearer real-secret"),
            "proxy did not inject upstream credential"
        );
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 27\r\nConnection: close\r\n\r\ndata: first\n\ndata: second\n\n",
            )
            .await?;
        stream.shutdown().await?;
        Ok::<_, color_eyre::Report>(())
    });

    let root = std::env::temp_dir().join(format!("kraai-eval-proxy-{}", ulid::Ulid::generate()));
    fs::create_dir(&root)?;
    let log_path = root.join("proxy.events.jsonl");
    let proxy = ModelProxy::start(ProxyServerConfig {
        upstream: format!("http://{upstream_address}"),
        credentials: UpstreamCredentials::OpenAiApiKey {
            credential: String::from("real-secret"),
            credential_env: String::from("TEST_API_KEY"),
        },
        allowed_paths: BTreeSet::from([String::from("/v1/chat/completions")]),
        kind: String::from("openai"),
        base_path: String::from("/v1"),
        log_path: log_path.clone(),
        max_requests: 1,
    })?;
    let client = Client::new();

    let unauthorized = client
        .post(format!("{}/chat/completions", proxy.base_url()))
        .send()
        .await?;
    ensure!(
        unauthorized.status() == 401,
        "unauthorized request was accepted"
    );

    let forbidden = client
        .get(format!("http://{}/v1/models", proxy.address))
        .bearer_auth(&proxy.token)
        .send()
        .await?;
    ensure!(forbidden.status() == 404, "unallowed path was forwarded");

    let response = client
        .post(format!("{}/chat/completions", proxy.base_url()))
        .bearer_auth(&proxy.token)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await?;
    ensure!(response.status() == 200, "allowed request failed");
    ensure!(
        response.text().await? == "data: first\n\ndata: second\n\n",
        "streamed response changed"
    );
    let limited = client
        .post(format!("{}/chat/completions", proxy.base_url()))
        .bearer_auth(&proxy.token)
        .body("{}")
        .send()
        .await?;
    ensure!(
        limited.status() == 429,
        "proxy request budget was not enforced"
    );
    let metrics_handle = Arc::clone(&proxy.metrics);
    upstream_task.await??;
    drop(proxy);
    let metrics = metrics_handle
        .lock()
        .map_err(|error| color_eyre::eyre::eyre!("proxy metrics mutex poisoned: {error}"))?
        .clone();
    ensure!(
        metrics.requests == 4,
        "proxy request count was not captured"
    );
    ensure!(
        metrics.successful_requests == 1 && metrics.failed_requests == 3,
        "proxy status metrics were not captured"
    );
    let log = fs::read_to_string(log_path)?;
    ensure!(
        log.contains("/v1/chat/completions"),
        "proxy request was not logged"
    );
    ensure!(
        !log.contains("real-secret"),
        "upstream credential leaked into logs"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn finish_drains_in_flight_response_before_snapshotting_metrics() -> Result<()> {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let upstream_address = upstream.local_addr()?;
    let (accepted_tx, accepted_rx) = oneshot::channel();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await?;
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                bail!("upstream client disconnected before headers");
            }
            request.extend_from_slice(chunk.get(..read).unwrap_or_default());
            if find_header_end(&request).is_some() {
                break;
            }
        }
        let _ = accepted_tx.send(());
        tokio::time::sleep(Duration::from_millis(50)).await;
        let body = b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"total_tokens\":30,\"input_tokens\":20,\"output_tokens\":10}}}\n\ndata: [DONE]\n\n";
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await?;
        stream.write_all(body).await?;
        stream.shutdown().await?;
        Ok::<_, color_eyre::Report>(())
    });

    let root = std::env::temp_dir().join(format!(
        "kraai-eval-proxy-finish-drain-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root)?;
    let log_path = root.join("proxy.events.jsonl");
    let proxy = ModelProxy::start(ProxyServerConfig {
        upstream: format!("http://{upstream_address}"),
        credentials: UpstreamCredentials::OpenAiApiKey {
            credential: String::from("real-secret"),
            credential_env: String::from("TEST_API_KEY"),
        },
        allowed_paths: BTreeSet::from([String::from("/v1/responses")]),
        kind: String::from("openai"),
        base_path: String::from("/v1"),
        log_path: log_path.clone(),
        max_requests: 1,
    })?;
    let mut downstream = std::net::TcpStream::connect(proxy.address)?;
    downstream.write_all(
        format!(
            "POST /v1/responses HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}",
            proxy.address, proxy.token
        )
        .as_bytes(),
    )?;
    accepted_rx.await?;

    let metrics = tokio::task::spawn_blocking(move || proxy.finish()).await??;
    upstream_task.await??;
    ensure!(metrics.requests == 1 && metrics.successful_requests == 1);
    ensure!(metrics.usage.total_tokens == 30);
    ensure!(fs::read_to_string(&log_path)?.contains("/v1/responses"));
    drop(downstream);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn client_disconnect_still_records_usage_metrics_and_event() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-proxy-disconnect-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root)?;
    let log_path = root.join("proxy.events.jsonl");
    let metrics = Arc::new(Mutex::new(ProxyMetrics::default()));
    let state = ProxyState {
        upstream: String::from("http://proxy-test.invalid"),
        credentials: UpstreamCredentials::OpenAiApiKey {
            credential: String::from("real-secret"),
            credential_env: String::from("TEST_API_KEY"),
        },
        allowed_paths: BTreeSet::from([String::from("/v1/responses")]),
        token: String::from("client-token"),
        client: Client::builder().redirect(Policy::none()).build()?,
        log: Arc::new(Mutex::new(File::create(&log_path)?)),
        max_requests: 1,
        request_count: AtomicU64::new(0),
        metrics: Arc::clone(&metrics),
    };
    let request = ParsedRequest {
        method: String::from("POST"),
        target: String::from("/v1/responses"),
        path: String::from("/v1/responses"),
        headers: vec![
            (
                String::from("authorization"),
                HeaderValue::from_static("Bearer client-token"),
            ),
            (
                String::from("content-type"),
                HeaderValue::from_static("application/json"),
            ),
        ],
        body: b"{}".to_vec(),
    };
    let (mut downstream, downstream_peer) = tokio::io::duplex(64);
    drop(downstream_peer);
    let body = futures::stream::iter(vec![
        Ok::<_, io::Error>(b"data: {\"type\":\"response.".to_vec()),
        Ok(b"completed\",\"response\":{\"usage\":{\"total_tokens\":30,\"input_tokens\":20,\"output_tokens\":10}}}\n\ndata: [DONE]\n\n".to_vec()),
    ]);

    let relayed = relay_response(&mut downstream, DownstreamDelivery::Complete, body).await?;
    let usage = record_usage_metrics(&state, &relayed.body)?;
    let outcome = ForwardOutcome {
        status: 200,
        delivery: relayed.delivery,
        usage,
        response_cache_state: CacheState::default(),
    };
    ensure!(
        outcome.delivery == DownstreamDelivery::ClientDisconnected,
        "closed downstream was not detected"
    );
    record_request_metrics(&state, &outcome, Duration::from_millis(5))?;
    write_event(&state, &request, &outcome, Duration::from_millis(5))?;

    let captured = metrics
        .lock()
        .map_err(|error| color_eyre::eyre::eyre!("proxy metrics mutex poisoned: {error}"))?
        .clone();
    ensure!(captured.requests == 1 && captured.successful_requests == 1);
    ensure!(captured.client_disconnects == 1);
    ensure!(captured.usage.total_tokens == 30);
    let log = fs::read_to_string(log_path)?;
    ensure!(log.contains("\"delivery\":\"client_disconnected\""));
    ensure!(log.contains("\"path\":\"/v1/responses\""));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn extracts_and_normalizes_responses_usage() {
    let body = br#"data: {"type":"response.completed","response":{"usage":{"total_tokens":165,"input_tokens":120,"output_tokens":45,"input_tokens_details":{"cached_tokens":20},"output_tokens_details":{"reasoning_tokens":5}}}}

data: [DONE]

"#;
    assert_eq!(
        usage_from_response_body(body),
        Some(UsageMetrics {
            total_tokens: 165,
            input_tokens: 100,
            output_tokens: 40,
            reasoning_tokens: 5,
            cache_read_tokens: 20,
        })
    );
}

#[test]
fn extracts_and_normalizes_chat_completions_usage() {
    let body = br#"{"usage":{"total_tokens":75,"prompt_tokens":50,"completion_tokens":25,"prompt_tokens_details":{"cached_tokens":10},"completion_tokens_details":{"reasoning_tokens":4}}}"#;
    assert_eq!(
        usage_from_response_body(body),
        Some(UsageMetrics {
            total_tokens: 75,
            input_tokens: 40,
            output_tokens: 21,
            reasoning_tokens: 4,
            cache_read_tokens: 10,
        })
    );
}

#[tokio::test]
async fn codex_proxy_keeps_subscription_tokens_outside_client_and_adds_account_headers()
-> Result<()> {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let upstream_address = upstream.local_addr()?;
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await?;
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                bail!("upstream client disconnected before headers");
            }
            request.extend_from_slice(chunk.get(..read).unwrap_or_default());
            if find_header_end(&request).is_some() {
                break;
            }
        }
        let request = String::from_utf8(request)?;
        ensure!(
            request.contains("authorization: Bearer subscription-access")
                || request.contains("Authorization: Bearer subscription-access"),
            "subscription access token was not injected"
        );
        ensure!(
            request.contains("chatgpt-account-id: account-123")
                || request.contains("ChatGPT-Account-Id: account-123"),
            "subscription account header was not injected"
        );
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
            )
            .await?;
        stream.shutdown().await?;
        Ok::<_, color_eyre::Report>(())
    });

    let root =
        std::env::temp_dir().join(format!("kraai-eval-codex-proxy-{}", ulid::Ulid::generate()));
    fs::create_dir(&root)?;
    let auth_path = root.join("auth.json");
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_account_id": "account-123"
        }
    }))?);
    fs::write(
        &auth_path,
        serde_json::to_vec(&serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": format!("e30.{payload}.signature"),
                "access_token": "subscription-access",
                "refresh_token": "subscription-refresh",
                "account_id": "account-123"
            },
            "last_refresh": SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
            "generation": "test-generation"
        }))?,
    )?;
    let controller = OpenAiCodexAuthController::new_with_options(
        OpenAiCodexAuthControllerOptions::new(auth_path),
    )?;
    let log_path = root.join("proxy.events.jsonl");
    let proxy = ModelProxy::start(ProxyServerConfig {
        upstream: format!("http://{upstream_address}"),
        credentials: UpstreamCredentials::Codex {
            controller,
            account_id: String::from("account-123"),
        },
        allowed_paths: codex_allowed_paths(),
        kind: String::from("openai-codex"),
        base_path: String::from("/backend-api"),
        log_path: log_path.clone(),
        max_requests: 1,
    })?;
    let response = Client::new()
        .post(format!("{}/codex/responses", proxy.base_url()))
        .bearer_auth(&proxy.token)
        .body("{}")
        .send()
        .await?;
    ensure!(response.status() == 200, "Codex proxy request failed");
    ensure!(response.text().await? == "{}", "Codex response changed");
    upstream_task.await??;
    drop(proxy);
    let log = fs::read_to_string(log_path)?;
    ensure!(
        !log.contains("subscription-access"),
        "access token leaked into logs"
    );
    ensure!(
        !log.contains("subscription-refresh"),
        "refresh token leaked into logs"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}
