use super::*;

fn start_proxy(limits: ConnectionLimits) -> Result<(PathBuf, ModelProxy)> {
    let root = std::env::temp_dir().join(format!("kraai-proxy-limits-{}", ulid::Ulid::generate()));
    fs::create_dir(&root)?;
    let proxy = ModelProxy::start(ProxyServerConfig {
        listen: ProxyListen::loopback(),
        upstream: String::from("http://proxy-test.invalid"),
        credentials: UpstreamCredentials::OpenAiApiKey {
            credential: String::from("real-secret"),
            credential_env: String::from("TEST_API_KEY"),
        },
        allowed_paths: BTreeSet::from([String::from("/v1/responses")]),
        kind: String::from("openai"),
        base_path: String::from("/v1"),
        log_path: root.join("proxy.events.jsonl"),
        max_requests: 1,
        shutdown_drain_timeout: kraai_test_support::HANG_TIMEOUT,
        limits,
    })?;
    Ok((root, proxy))
}

async fn response(stream: &mut TcpStream) -> Result<String> {
    let mut response = Vec::new();
    kraai_test_support::hang_guard(stream.read_to_end(&mut response)).await??;
    Ok(String::from_utf8(response)?)
}

#[tokio::test]
async fn partial_requests_are_dropped_after_the_read_deadline() -> Result<()> {
    let (root, proxy) = start_proxy(ConnectionLimits {
        request_read_timeout: Duration::from_millis(100),
        ..ConnectionLimits::default()
    })?;
    let partial_body = format!(
        "POST /v1/responses HTTP/1.1\r\nAuthorization: Bearer {}\r\nContent-Length: 1000\r\n\r\n{{\"model\"",
        proxy.token
    );
    for partial in [
        "POST /v1/responses HTTP/1.1\r\nContent-Length: 2\r\n",
        partial_body.as_str(),
    ] {
        let mut stream = TcpStream::connect(proxy.address).await?;
        stream.write_all(partial.as_bytes()).await?;
        ensure!(response(&mut stream).await?.starts_with("HTTP/1.1 408 "));
    }
    let metrics = tokio::task::spawn_blocking(move || proxy.finish()).await??;
    ensure!(metrics.requests == 0 && metrics.unrecorded_requests == 0);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn unauthorized_requests_are_rejected_before_the_body_arrives() -> Result<()> {
    let (root, proxy) = start_proxy(ConnectionLimits::default())?;
    let mut stream = TcpStream::connect(proxy.address).await?;
    stream
        .write_all(b"POST /v1/responses HTTP/1.1\r\nContent-Length: 33554432\r\n\r\n")
        .await?;
    ensure!(response(&mut stream).await?.starts_with("HTTP/1.1 401 "));
    drop(stream);
    let metrics = tokio::task::spawn_blocking(move || proxy.finish()).await??;
    ensure!(metrics.requests == 1 && metrics.failed_requests == 1);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn connections_past_the_cap_wait_for_a_free_slot() -> Result<()> {
    let (root, proxy) = start_proxy(ConnectionLimits {
        max_connections: 1,
        ..ConnectionLimits::default()
    })?;
    let mut held = TcpStream::connect(proxy.address).await?;
    held.write_all(b"GET /v1/responses HTTP/1.1\r\n").await?;
    let mut waiting = TcpStream::connect(proxy.address).await?;
    waiting
        .write_all(b"GET /v1/models HTTP/1.1\r\n\r\n")
        .await?;
    let mut byte = [0_u8; 1];
    ensure!(
        tokio::time::timeout(Duration::from_millis(100), waiting.read(&mut byte))
            .await
            .is_err(),
        "proxy served a connection past its cap"
    );
    held.shutdown().await?;
    ensure!(response(&mut held).await?.starts_with("HTTP/1.1 400 "));
    ensure!(response(&mut waiting).await?.starts_with("HTTP/1.1 404 "));
    drop(proxy);
    fs::remove_dir_all(root)?;
    Ok(())
}
