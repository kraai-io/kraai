use super::*;

fn fixture() -> Result<(PathBuf, ProxyState, ParsedRequest)> {
    let root = std::env::temp_dir().join(format!("kraai-proxy-failure-{}", ulid::Ulid::generate()));
    fs::create_dir(&root)?;
    let state = ProxyState {
        upstream: String::from("http://upstream.invalid"),
        credentials: UpstreamCredentials::OpenAiApiKey {
            credential: String::from("upstream-secret"),
            credential_env: String::from("TEST_API_KEY"),
        },
        allowed_paths: BTreeSet::from([String::from("/v1/responses")]),
        token: String::from("client-token"),
        client: Client::builder().redirect(Policy::none()).build()?,
        log: Arc::new(Mutex::new(File::create(root.join("proxy.events.jsonl"))?)),
        max_requests: 1,
        request_count: AtomicU64::new(0),
        started_requests: AtomicU64::new(0),
        metrics: Arc::new(Mutex::new(ProxyMetrics::default())),
    };
    let request = ParsedRequest {
        method: String::from("POST"),
        target: String::from("/v1/responses"),
        path: String::from("/v1/responses"),
        headers: vec![(
            String::from("authorization"),
            HeaderValue::from_static("Bearer client-token"),
        )],
        body: br#"{"model":"model","input":"private prompt"}"#.to_vec().into(),
    };
    Ok((root, state, request))
}

fn field<'a>(event: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    event.pointer(path).unwrap_or(&serde_json::Value::Null)
}

fn read_event(root: &std::path::Path) -> Result<serde_json::Value> {
    let log = fs::read_to_string(root.join("proxy.events.jsonl"))?;
    ensure!(log.lines().count() == 1);
    for secret in ["upstream-secret", "client-token", "private prompt"] {
        ensure!(!log.contains(secret));
    }
    let event: serde_json::Value = serde_json::from_str(log.trim())?;
    ensure!(
        field(&event, "/request_id")
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    ensure!(field(&event, "/duration_ms").is_number());
    Ok(event)
}

#[tokio::test]
async fn truncated_http_response_records_original_status_and_error_chain() -> Result<()> {
    let (root, mut state, _) = fixture()?;
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    state.upstream = format!("http://{}", upstream.local_addr()?);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let address = listener.local_addr()?;
    let state = Arc::new(state);
    let serve = async {
        let (stream, _) = listener.accept().await?;
        handle_connection(stream, Arc::clone(&state)).await
    };
    let respond = async {
        let (mut socket, _) = upstream.accept().await?;
        read_request(&mut socket).await?;
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nx-request-id: upstream-request\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n:\n\n\r\n").await?;
        socket.shutdown().await?;
        Ok::<_, color_eyre::Report>(())
    };
    let receive = async {
        let response = Client::new()
            .post(format!("http://{address}/v1/responses"))
            .bearer_auth("client-token")
            .body(r#"{"model":"model"}"#)
            .send()
            .await?;
        ensure!(response.status() == 200);
        let body = response.bytes().await;
        ensure!(body.is_err());
        Ok::<_, color_eyre::Report>(())
    };
    let (served, responded, received) =
        Box::pin(tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(serve, respond, receive)
        }))
        .await?;
    ensure!(served.is_err());
    responded?;
    received?;
    let event = read_event(&root)?;
    ensure!(
        field(&event, "/status") == 200
            && field(&event, "/upstream_request_id") == "upstream-request"
    );
    ensure!(
        field(&event, "/delivery") == "incomplete" && field(&event, "/stage") == "upstream_body"
    );
    let error = field(&event, "/error").as_str().unwrap_or_default();
    ensure!(error.contains("reading upstream response body") && error.contains("unexpected EOF"));
    let metrics = state
        .metrics
        .lock()
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?
        .clone();
    ensure!(
        metrics.requests == 1 && metrics.failed_requests == 1 && metrics.successful_requests == 0
    );
    ensure!(metrics.unrecorded_requests == 0);
    drop(state);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn timeout_and_shutdown_preserve_partial_usage_without_counting_twice() -> Result<()> {
    for cancelled in [false, true] {
        let (root, state, request) = fixture()?;
        let mut record = RequestRecord::new(&state, &request, Instant::now());
        record.outcome.status = Some(200);
        record.outcome.delivery = DownstreamDelivery::Complete;
        let payload = b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"total_tokens\":30,\"input_tokens\":20,\"output_tokens\":10}}}\n\n".to_vec();
        let body = futures::stream::iter(vec![Ok::<_, io::Error>(payload)]);
        let mut downstream = tokio::io::sink();
        if cancelled {
            let stalled = futures::StreamExt::chain(body, futures::stream::pending());
            let result = tokio::time::timeout(
                Duration::from_millis(10),
                relay_response(&mut downstream, &mut record.outcome, stalled),
            )
            .await;
            ensure!(result.is_err());
        } else {
            let failing = futures::StreamExt::chain(
                body,
                futures::stream::once(async {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "injected read timeout",
                    ))
                }),
            );
            let result = relay_response(&mut downstream, &mut record.outcome, failing).await;
            let recorded = record.finish(result);
            ensure!(recorded.is_err());
        }
        drop(record);
        let event = read_event(&root)?;
        ensure!(field(&event, "/status") == 200 && field(&event, "/delivery") == "incomplete");
        ensure!(
            field(&event, "/stage") == "upstream_body"
                && field(&event, "/usage/total_tokens") == 30
        );
        let error = field(&event, "/error").as_str().unwrap_or_default();
        ensure!(error.contains(if cancelled {
            "cancelled"
        } else {
            "injected read timeout"
        }));
        let metrics = state
            .metrics
            .lock()
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?
            .clone();
        ensure!(
            metrics.requests == 1
                && metrics.failed_requests == 1
                && metrics.usage.total_tokens == 30
        );
        drop(state);
        fs::remove_dir_all(root)?;
    }
    Ok(())
}
