mod failures;
mod routing;

use super::forwarding::relay_response;

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
        ensure!(request.body.as_ref() == b"{}");
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

#[tokio::test]
async fn request_header_scanning_preserves_every_split_and_the_size_limit() -> Result<()> {
    let raw = b"POST /v1/responses?mode=test HTTP/1.1\r\nContent-Length: 2\r\nX-State: opaque-\xff\r\n\r\n{}ignored";
    let expected = read_request(&mut raw.as_slice()).await?;
    for split in 0..=raw.len() {
        let mut input = AsyncReadExt::chain(
            raw.get(..split).unwrap_or_default(),
            raw.get(split..).unwrap_or_default(),
        );
        let actual = read_request(&mut input).await?;
        ensure!(actual.method == expected.method);
        ensure!(actual.target == expected.target);
        ensure!(actual.path == expected.path);
        ensure!(actual.headers == expected.headers);
        ensure!(actual.body == expected.body);
    }

    let mut large = b"GET /v1/models HTTP/1.1\r\nX-Fill: ".to_vec();
    large.resize(MAX_HEADER_BYTES - 4, b'x');
    large.extend_from_slice(b"\r\n\r\n");
    for split in (MAX_HEADER_BYTES - 7)..=MAX_HEADER_BYTES {
        let mut input = AsyncReadExt::chain(
            large.get(..split).unwrap_or_default(),
            large.get(split..).unwrap_or_default(),
        );
        ensure!(read_request(&mut input).await?.path == "/v1/models");
    }
    large.truncate(MAX_HEADER_BYTES - 4);
    large.extend_from_slice(b"xxxx\r\n\r\n");
    let error = read_request(&mut large.as_slice())
        .await
        .err()
        .ok_or_else(|| color_eyre::eyre::eyre!("oversized headers were accepted"))?;
    ensure!(error.to_string() == "proxy request headers exceed limit");
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

use super::credentials::codex_allowed_paths;
use super::request::{MAX_HEADER_BYTES, find_header_end};
use super::*;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use color_eyre::eyre::ensure;
use kraai_provider_openai_codex::{OpenAiCodexAuthController, OpenAiCodexAuthControllerOptions};
use reqwest::header::HeaderValue;
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn listener_failure_drains_tasks_and_records_failed_requests() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-proxy-listener-error-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root)?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let address = listener.local_addr()?;
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let metrics = Arc::new(Mutex::new(ProxyMetrics::default()));
    let state = Arc::new(ProxyState {
        upstream: format!("http://{}", upstream.local_addr()?),
        credentials: UpstreamCredentials::OpenAiApiKey {
            credential: String::from("real-secret"),
            credential_env: String::from("TEST_API_KEY"),
        },
        allowed_paths: BTreeSet::from([String::from("/v1/responses")]),
        token: String::from("client-token"),
        client: Client::builder().redirect(Policy::none()).build()?,
        log: Arc::new(Mutex::new(File::create(root.join("proxy.events.jsonl"))?)),
        max_requests: 1,
        request_count: AtomicU64::new(0),
        started_requests: AtomicU64::new(0),
        metrics: Arc::clone(&metrics),
    });
    let (upstream_started, upstream_ready) = tokio::sync::watch::channel(false);
    let (response_started, mut response_ready) = tokio::sync::watch::channel(false);
    let upstream_request = async {
        let (mut stream, _) = upstream.accept().await?;
        read_request(&mut stream).await?;
        upstream_started.send_replace(true);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nx")
            .await?;
        response_ready.wait_for(|ready| *ready).await?;
        stream.shutdown().await?;
        Ok::<_, color_eyre::Report>(())
    };
    let mut accepted = false;
    let accept = || {
        let first = !std::mem::replace(&mut accepted, true);
        let listener = &listener;
        let mut upstream_ready = upstream_ready.clone();
        async move {
            if first {
                listener.accept().await
            } else {
                upstream_ready
                    .wait_for(|ready| *ready)
                    .await
                    .map_err(io::Error::other)?;
                Err(io::Error::other("injected listener failure"))
            }
        }
    };
    let (_shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = serve_connections(accept, Arc::clone(&state), shutdown_rx);
    let client = async {
        let mut stream = TcpStream::connect(address).await?;
        stream.write_all(b"POST /v1/responses HTTP/1.1\r\nAuthorization: Bearer client-token\r\nContent-Length: 2\r\n\r\n{}").await?;
        let mut response = Vec::new();
        while find_header_end(&response).is_none() {
            let mut buffer = [0_u8; 1024];
            let count = stream.read(&mut buffer).await?;
            ensure!(
                count != 0,
                "proxy closed before forwarding the response headers"
            );
            response.extend_from_slice(buffer.get(..count).unwrap_or_default());
        }
        ensure!(response.starts_with(b"HTTP/1.1 200"));
        response_started.send_replace(true);
        stream.read_to_end(&mut response).await?;
        Ok::<_, color_eyre::Report>(())
    };
    let (server, upstream, client) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(server, upstream_request, client)
    })
    .await?;
    upstream?;
    client?;
    let error = server
        .err()
        .ok_or_else(|| color_eyre::eyre::eyre!("listener failure was lost"))?;
    ensure!(error.to_string() == "injected listener failure");
    ensure!(state.started_requests.load(Ordering::Relaxed) == 1);
    let captured = metrics
        .lock()
        .map_err(|error| color_eyre::eyre::eyre!("fixture metrics mutex poisoned: {error}"))?
        .clone();
    ensure!(
        captured.requests == 1
            && captured.failed_requests == 1
            && captured.unrecorded_requests == 0
    );
    drop(state);
    fs::remove_dir_all(root)?;
    Ok(())
}

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
        listen_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
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
        listen_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
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
async fn rejected_requests_cannot_hide_model_calls_aborted_during_shutdown() -> Result<()> {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let upstream_address = upstream.local_addr()?;
    let (accepted_tx, accepted_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let upstream_task = tokio::spawn(async move {
        let (mut completed, _) = upstream.accept().await?;
        read_request(&mut completed).await?;
        let body = br#"{"model":"model","usage":{"total_tokens":30,"input_tokens":20,"output_tokens":10}}"#;
        completed
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await?;
        completed.write_all(body).await?;
        completed.shutdown().await?;
        let (mut stalled, _) = upstream.accept().await?;
        read_request(&mut stalled).await?;
        let _ = accepted_tx.send(());
        release_rx.await?;
        drop(stalled);
        Ok::<_, color_eyre::Report>(())
    });

    let root = std::env::temp_dir().join(format!(
        "kraai-eval-proxy-aborted-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root)?;
    let pricing_config = root.join("prices.toml");
    fs::write(
        &pricing_config,
        r#"
[[provider]]
id = "test"
type = "custom"
[[model]]
id = "model"
provider_id = "test"
price_input = "2"
price_output = "8"
"#,
    )?;
    let mut proxy = ModelProxy::start(ProxyServerConfig {
        listen_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        upstream: format!("http://{upstream_address}"),
        credentials: UpstreamCredentials::OpenAiApiKey {
            credential: String::from("real-secret"),
            credential_env: String::from("TEST_API_KEY"),
        },
        allowed_paths: BTreeSet::from([String::from("/v1/responses")]),
        kind: String::from("openai"),
        base_path: String::from("/v1"),
        log_path: root.join("proxy.events.jsonl"),
        max_requests: 2,
    })?;
    proxy.pricing.config = Some(pricing_config);
    let client = Client::new();
    let rejected = client
        .get(format!("http://{}/v1/models", proxy.address))
        .send()
        .await?;
    ensure!(rejected.status() == 404);
    rejected.bytes().await?;
    let completed = client
        .post(format!("http://{}/v1/responses", proxy.address))
        .bearer_auth(&proxy.token)
        .body(r#"{"model":"model"}"#)
        .send()
        .await?;
    ensure!(completed.status() == 200);
    completed.bytes().await?;
    let mut downstream = TcpStream::connect(proxy.address).await?;
    downstream
        .write_all(
            format!(
                "POST /v1/responses HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}",
                proxy.address, proxy.token
            )
            .as_bytes(),
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(10), accepted_rx).await??;

    let metrics = tokio::task::spawn_blocking(move || proxy.finish()).await??;
    let _ = release_tx.send(());
    upstream_task.await??;
    ensure!(
        metrics.requests == 3 && metrics.successful_requests == 1 && metrics.failed_requests == 2
    );
    ensure!(metrics.unrecorded_requests == 0);
    let accounting = metrics
        .accounting
        .ok_or_else(|| color_eyre::eyre::eyre!("missing request accounting"))?;
    ensure!(accounting.priced_requests == 1 && accounting.context.samples == 1);
    ensure!(accounting.unrecorded_requests == 0 && accounting.unpriced_requests == 1);
    ensure!(accounting.complete_cost().is_none() && accounting.complete_context().is_none());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn accounting_errors_preserve_measured_proxy_metrics() -> Result<()> {
    for analysis_error in [true, false] {
        let root = std::env::temp_dir().join(format!(
            "kraai-eval-proxy-accounting-error-{}",
            ulid::Ulid::generate()
        ));
        fs::create_dir(&root)?;
        let log_path = root.join("proxy.events.jsonl");
        let mut proxy = ModelProxy::start(ProxyServerConfig {
            listen_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            upstream: String::from("http://proxy-test.invalid"),
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
        let rejected = Client::new()
            .get(format!("http://{}/v1/models", proxy.address))
            .send()
            .await?;
        ensure!(rejected.status() == 404);
        rejected.bytes().await?;
        proxy.shutdown_and_join();
        if analysis_error {
            fs::write(&log_path, "invalid request event\n")?;
        } else {
            fs::create_dir(root.join("request-accounting.json"))?;
        }

        let metrics = proxy.finish()?;
        ensure!(metrics.requests == 1 && metrics.failed_requests == 1);
        ensure!(metrics.accounting.is_none() && metrics.accounting_error.is_some());
        let diagnostic: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("request-accounting-error.json"))?)?;
        ensure!(
            diagnostic.get("error").and_then(serde_json::Value::as_str)
                == metrics.accounting_error.as_deref()
        );
        fs::remove_dir_all(root)?;
    }
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
        started_requests: AtomicU64::new(0),
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
        body: b"{}".to_vec().into(),
    };
    let (mut downstream, downstream_peer) = tokio::io::duplex(64);
    drop(downstream_peer);
    let body = futures::stream::iter(vec![
        Ok::<_, io::Error>(b"data: {\"type\":\"response.".to_vec()),
        Ok(b"completed\",\"response\":{\"usage\":{\"total_tokens\":30,\"input_tokens\":20,\"output_tokens\":10}}}\n\ndata: [DONE]\n\n".to_vec()),
    ]);

    let mut outcome = ForwardOutcome::rejected(200);
    relay_response(&mut downstream, &mut outcome, body).await?;
    outcome.usage = record_usage_metrics(&state, &outcome.body)?;
    ensure!(
        outcome.delivery == DownstreamDelivery::ClientDisconnected,
        "closed downstream was not detected"
    );
    record_request_metrics(&state, &outcome, Duration::from_millis(5))?;
    write_event(
        &state,
        &request,
        "test-request",
        &outcome,
        Duration::from_millis(5),
    )?;

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
            cache_write_tokens: 0,
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
            cache_write_tokens: 0,
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
        listen_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
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
