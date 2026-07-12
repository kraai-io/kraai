use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Context, Result, bail};
use futures::StreamExt;
use kraai_provider_openai_codex::OpenAiCodexAuthController;
use reqwest::redirect::Policy;
use reqwest::{Client, Method};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::ProxyRecord;
use crate::metrics::{ProxyMetrics, UsageMetrics};

const OPENAI_UPSTREAM: &str = "https://api.openai.com";
const CHATGPT_UPSTREAM: &str = "https://chatgpt.com";
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ModelProxyRequest {
    credentials: ProxyCredentialRequest,
    max_requests: u64,
}

#[derive(Debug, Clone)]
enum ProxyCredentialRequest {
    OpenAiApiKey { credential_env: String },
    CodexSubscription,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelProxyIdentity {
    kind: String,
    upstream: String,
    allowed_paths: Vec<String>,
    credential_source: String,
    credential_sha256: String,
    max_requests: u64,
}

impl ModelProxyRequest {
    pub fn openai(credential_env: String, max_requests: u64) -> Self {
        Self {
            credentials: ProxyCredentialRequest::OpenAiApiKey { credential_env },
            max_requests,
        }
    }

    pub fn codex_subscription(max_requests: u64) -> Self {
        Self {
            credentials: ProxyCredentialRequest::CodexSubscription,
            max_requests,
        }
    }

    pub(crate) fn is_codex_subscription(&self) -> bool {
        matches!(self.credentials, ProxyCredentialRequest::CodexSubscription)
    }

    pub(crate) fn identity(&self) -> Result<ModelProxyIdentity> {
        let resolved = self.resolve_credentials()?;
        Ok(ModelProxyIdentity {
            kind: resolved.kind().to_string(),
            upstream: resolved.upstream().to_string(),
            allowed_paths: resolved.allowed_paths().into_iter().collect(),
            credential_source: resolved.credential_source(),
            credential_sha256: resolved.fingerprint(),
            max_requests: self.max_requests,
        })
    }

    pub(crate) fn start(&self, log_path: PathBuf) -> Result<ModelProxy> {
        if self.max_requests == 0 {
            bail!("model proxy max_requests must be greater than zero");
        }
        let credentials = self.resolve_credentials()?;
        ModelProxy::start(ProxyServerConfig {
            upstream: credentials.upstream().to_string(),
            allowed_paths: credentials.allowed_paths(),
            kind: credentials.kind().to_string(),
            base_path: credentials.base_path().to_string(),
            credentials,
            log_path,
            max_requests: self.max_requests,
        })
    }

    fn resolve_credentials(&self) -> Result<UpstreamCredentials> {
        match &self.credentials {
            ProxyCredentialRequest::OpenAiApiKey { credential_env } => {
                let credential = std::env::var(credential_env).wrap_err_with(|| {
                    format!(
                        "model proxy credential environment variable {credential_env} is unavailable"
                    )
                })?;
                if credential.trim().is_empty() {
                    bail!("model proxy credential must not be empty");
                }
                Ok(UpstreamCredentials::OpenAiApiKey {
                    credential,
                    credential_env: credential_env.clone(),
                })
            }
            ProxyCredentialRequest::CodexSubscription => codex_credentials(),
        }
    }
}

#[derive(Clone)]
enum UpstreamCredentials {
    OpenAiApiKey {
        credential: String,
        credential_env: String,
    },
    Codex {
        controller: OpenAiCodexAuthController,
        account_id: String,
    },
}

impl UpstreamCredentials {
    fn kind(&self) -> &'static str {
        match self {
            Self::OpenAiApiKey { .. } => "openai",
            Self::Codex { .. } => "openai-codex",
        }
    }

    fn upstream(&self) -> &'static str {
        match self {
            Self::OpenAiApiKey { .. } => OPENAI_UPSTREAM,
            Self::Codex { .. } => CHATGPT_UPSTREAM,
        }
    }

    fn base_path(&self) -> &'static str {
        match self {
            Self::OpenAiApiKey { .. } => "/v1",
            Self::Codex { .. } => "/backend-api",
        }
    }

    fn allowed_paths(&self) -> BTreeSet<String> {
        match self {
            Self::OpenAiApiKey { .. } => openai_allowed_paths(),
            Self::Codex { .. } => codex_allowed_paths(),
        }
    }

    fn credential_source(&self) -> String {
        match self {
            Self::OpenAiApiKey { credential_env, .. } => format!("env:{credential_env}"),
            Self::Codex { account_id, .. } => format!("codex-account:{account_id}"),
        }
    }

    fn fingerprint(&self) -> String {
        let material = match self {
            Self::OpenAiApiKey { credential, .. } => credential.as_bytes(),
            Self::Codex { account_id, .. } => account_id.as_bytes(),
        };
        crate::cache::hash_chunks(&[material.to_vec()])
    }
}

fn codex_credentials() -> Result<UpstreamCredentials> {
    let controller = OpenAiCodexAuthController::new()?;
    let worker = controller.clone();
    let thread = std::thread::spawn(move || -> Result<String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let auth = runtime.block_on(worker.get_request_auth())?;
        Ok(auth.account_id().to_string())
    });
    let account_id = thread
        .join()
        .map_err(|_panic| color_eyre::eyre::eyre!("Codex authentication worker panicked"))??;
    Ok(UpstreamCredentials::Codex {
        controller,
        account_id,
    })
}

pub(crate) struct ModelProxy {
    address: SocketAddr,
    token: String,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    record: ProxyRecord,
    base_path: String,
    metrics: Arc<Mutex<ProxyMetrics>>,
}

impl ModelProxy {
    fn start(config: ProxyServerConfig) -> Result<Self> {
        let token = random_token()?;
        let metrics = Arc::new(Mutex::new(ProxyMetrics::default()));
        let identity_paths = config.allowed_paths.iter().cloned().collect::<Vec<_>>();
        let record = ProxyRecord {
            kind: config.kind.clone(),
            upstream: config.upstream.clone(),
            allowed_paths: identity_paths,
            max_requests: config.max_requests,
            credential_fingerprint: config.credentials.fingerprint(),
        };
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let thread_token = token.clone();
        let base_path = config.base_path.clone();
        let thread_metrics = Arc::clone(&metrics);
        let thread = std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(color_eyre::Report::from)
                .and_then(|runtime| {
                    runtime.block_on(run_server(
                        config,
                        thread_token,
                        thread_metrics,
                        shutdown_rx,
                        ready_tx,
                    ))
                });
            if let Err(error) = result {
                tracing_fallback(&error.to_string());
            }
        });
        let address = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .wrap_err("model proxy did not start")??;
        Ok(Self {
            address,
            token,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
            record,
            base_path,
            metrics,
        })
    }

    pub(crate) fn base_url(&self) -> String {
        self.url()
    }

    pub(crate) fn environment(&self) -> BTreeMap<String, String> {
        let base_url = self.url();
        if self.record.kind == "openai-codex" {
            BTreeMap::from([
                (
                    String::from("KRAAI_EVAL_CODEX_PROXY_TOKEN"),
                    self.token.clone(),
                ),
                (String::from("KRAAI_EVAL_CODEX_BASE_URL"), base_url),
            ])
        } else {
            BTreeMap::from([
                (String::from("OPENAI_API_KEY"), self.token.clone()),
                (String::from("OPENAI_BASE_URL"), base_url.clone()),
                (String::from("OPENAI_API_BASE"), base_url.clone()),
                (String::from("KRAAI_EVAL_OPENAI_BASE_URL"), base_url),
            ])
        }
    }

    pub(crate) fn record(&self) -> ProxyRecord {
        self.record.clone()
    }

    pub(crate) fn metrics(&self) -> Result<ProxyMetrics> {
        self.metrics
            .lock()
            .map(|metrics| metrics.clone())
            .map_err(|error| color_eyre::eyre::eyre!("proxy metrics mutex poisoned: {error}"))
    }

    fn url(&self) -> String {
        format!("http://{}{}", self.address, self.base_path)
    }
}

impl Drop for ModelProxy {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct ProxyServerConfig {
    upstream: String,
    credentials: UpstreamCredentials,
    allowed_paths: BTreeSet<String>,
    kind: String,
    base_path: String,
    log_path: PathBuf,
    max_requests: u64,
}

#[derive(Serialize)]
struct ProxyEvent<'a> {
    timestamp_ms: u128,
    method: &'a str,
    path: &'a str,
    status: u16,
    duration_ms: u128,
}

async fn run_server(
    config: ProxyServerConfig,
    token: String,
    metrics: Arc<Mutex<ProxyMetrics>>,
    mut shutdown: oneshot::Receiver<()>,
    ready: mpsc::SyncSender<Result<SocketAddr>>,
) -> Result<()> {
    let listener =
        match TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await {
            Ok(listener) => listener,
            Err(error) => {
                let _ = ready.send(Err(error.into()));
                return Ok(());
            }
        };
    let address = listener.local_addr()?;
    let log = Arc::new(Mutex::new(File::create(&config.log_path)?));
    let state = Arc::new(ProxyState {
        upstream: config.upstream,
        credentials: config.credentials,
        allowed_paths: config.allowed_paths,
        token,
        client: Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()?,
        log,
        max_requests: config.max_requests,
        request_count: AtomicU64::new(0),
        metrics,
    });
    if ready.send(Ok(address)).is_err() {
        return Ok(());
    }

    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let state = Arc::clone(&state);
                tasks.spawn(async move {
                    if let Err(error) = handle_connection(stream, state).await {
                        tracing_fallback(&error.to_string());
                    }
                });
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

struct ProxyState {
    upstream: String,
    credentials: UpstreamCredentials,
    allowed_paths: BTreeSet<String>,
    token: String,
    client: Client,
    log: Arc<Mutex<File>>,
    max_requests: u64,
    request_count: AtomicU64,
    metrics: Arc<Mutex<ProxyMetrics>>,
}

struct ParsedRequest {
    method: String,
    target: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

async fn handle_connection(mut stream: TcpStream, state: Arc<ProxyState>) -> Result<()> {
    let started = Instant::now();
    let request = match read_request(&mut stream).await {
        Ok(request) => request,
        Err(error) => {
            write_error(&mut stream, 400, "Bad Request").await?;
            return Err(error);
        }
    };
    let status = forward_request(&mut stream, &state, &request).await?;
    let duration = started.elapsed();
    record_request_metrics(&state, status, duration)?;
    write_event(&state, &request, status, duration)?;
    stream.shutdown().await?;
    Ok(())
}

async fn forward_request(
    stream: &mut TcpStream,
    state: &ProxyState,
    request: &ParsedRequest,
) -> Result<u16> {
    if !state.allowed_paths.contains(&request.path) {
        write_error(stream, 404, "Not Found").await?;
        return Ok(404);
    }
    let authorized = request
        .headers
        .iter()
        .find(|(name, _)| name == "authorization")
        .is_some_and(|(_, value)| constant_time_eq(value, &format!("Bearer {}", state.token)));
    if !authorized {
        write_error(stream, 401, "Unauthorized").await?;
        return Ok(401);
    }
    let method = Method::from_bytes(request.method.as_bytes())?;
    if !matches!(method, Method::GET | Method::POST) {
        write_error(stream, 405, "Method Not Allowed").await?;
        return Ok(405);
    }
    if state.request_count.fetch_add(1, Ordering::Relaxed) >= state.max_requests {
        write_error(stream, 429, "Proxy Request Limit Exceeded").await?;
        return Ok(429);
    }
    let response = match send_upstream(state, method, request).await {
        Ok(response) => response,
        Err(error) => {
            write_error(stream, 502, "Bad Gateway").await?;
            tracing_fallback(&format!("model proxy upstream request failed: {error}"));
            return Ok(502);
        }
    };
    let status = response.status();
    let reason = status.canonical_reason().unwrap_or("Upstream Response");
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream");
    stream
        .write_all(
            format!(
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                status.as_u16(), reason, content_type
            )
            .as_bytes(),
        )
        .await?;
    let mut body = response.bytes_stream();
    let mut response_bytes = 0_usize;
    let mut captured_body = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        response_bytes = response_bytes.saturating_add(chunk.len());
        if response_bytes > MAX_RESPONSE_BODY_BYTES {
            bail!("model proxy response body exceeds limit");
        }
        captured_body.extend_from_slice(&chunk);
        stream
            .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
            .await?;
        stream.write_all(&chunk).await?;
        stream.write_all(b"\r\n").await?;
    }
    stream.write_all(b"0\r\n\r\n").await?;
    if let Some(usage) = usage_from_response_body(&captured_body) {
        let mut metrics = state
            .metrics
            .lock()
            .map_err(|error| color_eyre::eyre::eyre!("proxy metrics mutex poisoned: {error}"))?;
        metrics.usage.accumulate(&usage);
    }
    Ok(status.as_u16())
}

fn record_request_metrics(state: &ProxyState, status: u16, duration: Duration) -> Result<()> {
    let mut metrics = state
        .metrics
        .lock()
        .map_err(|error| color_eyre::eyre::eyre!("proxy metrics mutex poisoned: {error}"))?;
    metrics.requests = metrics.requests.saturating_add(1);
    if (200..400).contains(&status) {
        metrics.successful_requests = metrics.successful_requests.saturating_add(1);
    } else {
        metrics.failed_requests = metrics.failed_requests.saturating_add(1);
    }
    metrics.duration_ms = metrics.duration_ms.saturating_add(duration.as_millis());
    drop(metrics);
    Ok(())
}

fn usage_from_response_body(body: &[u8]) -> Option<UsageMetrics> {
    let text = std::str::from_utf8(body).ok()?;
    let mut usage = None;
    for line in text.lines() {
        let payload = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
            continue;
        };
        if let Some(candidate) = usage_from_json(&value) {
            usage = Some(candidate);
        }
    }
    usage
}

fn usage_from_json(value: &serde_json::Value) -> Option<UsageMetrics> {
    let usage = value
        .get("response")
        .and_then(|response| response.get("usage"))
        .or_else(|| value.get("usage"))?;
    let cached = nested_u64(usage, "input_tokens_details", "cached_tokens")
        .or_else(|| nested_u64(usage, "prompt_tokens_details", "cached_tokens"))
        .unwrap_or_default();
    let reasoning = nested_u64(usage, "output_tokens_details", "reasoning_tokens")
        .or_else(|| nested_u64(usage, "completion_tokens_details", "reasoning_tokens"))
        .unwrap_or_default();
    let raw_input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let raw_output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let total = usage
        .get("total_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| raw_input.saturating_add(raw_output));
    (total != 0 || raw_input != 0 || raw_output != 0).then_some(UsageMetrics {
        total_tokens: total,
        input_tokens: raw_input.saturating_sub(cached),
        output_tokens: raw_output.saturating_sub(reasoning),
        reasoning_tokens: reasoning,
        cache_read_tokens: cached,
    })
}

fn nested_u64(value: &serde_json::Value, object: &str, field: &str) -> Option<u64> {
    value
        .get(object)
        .and_then(|details| details.get(field))
        .and_then(serde_json::Value::as_u64)
}

async fn send_upstream(
    state: &ProxyState,
    method: Method,
    request: &ParsedRequest,
) -> Result<reqwest::Response> {
    match &state.credentials {
        UpstreamCredentials::OpenAiApiKey { credential, .. } => {
            Ok(upstream_request(state, method, request)
                .bearer_auth(credential)
                .send()
                .await?)
        }
        UpstreamCredentials::Codex { controller, .. } => {
            let auth = controller.get_request_auth().await?;
            let response = auth
                .apply_chatgpt_headers(upstream_request(state, method.clone(), request))
                .send()
                .await?;
            if response.status() != reqwest::StatusCode::UNAUTHORIZED {
                return Ok(response);
            }
            let refreshed = controller.refresh_request_auth(&auth).await?;
            Ok(refreshed
                .apply_chatgpt_headers(upstream_request(state, method, request))
                .send()
                .await?)
        }
    }
}

fn upstream_request(
    state: &ProxyState,
    method: Method,
    request: &ParsedRequest,
) -> reqwest::RequestBuilder {
    let mut builder = state.client.request(
        method,
        format!("{}{}", state.upstream.trim_end_matches('/'), request.target),
    );
    for (name, value) in &request.headers {
        if matches!(
            name.as_str(),
            "accept"
                | "content-type"
                | "openai-beta"
                | "user-agent"
                | "session_id"
                | "x-client-request-id"
        ) {
            builder = builder.header(name, value);
        }
    }
    builder.body(request.body.clone())
}

async fn read_request(stream: &mut TcpStream) -> Result<ParsedRequest> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if bytes.len() >= MAX_HEADER_BYTES {
            bail!("proxy request headers exceed limit");
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            bail!("proxy client disconnected before request headers completed");
        }
        bytes.extend_from_slice(chunk.get(..read).unwrap_or_default());
        if let Some(position) = find_header_end(&bytes) {
            break position;
        }
    };
    let header_text = std::str::from_utf8(bytes.get(..header_end).unwrap_or_default())?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing request line"))?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_owned();
    let target = request_parts.next().unwrap_or_default().to_owned();
    let version = request_parts.next().unwrap_or_default();
    if method.is_empty() || !target.starts_with('/') || version != "HTTP/1.1" {
        bail!("invalid proxy request line");
    }
    let path = target.split('?').next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    let mut content_length = 0_usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            bail!("invalid proxy request header");
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();
        if name == "transfer-encoding" {
            bail!("chunked proxy requests are not supported");
        }
        if name == "content-length" {
            content_length = value.parse()?;
            if content_length > MAX_REQUEST_BODY_BYTES {
                bail!("proxy request body exceeds limit");
            }
        }
        headers.push((name, value));
    }
    let body_start = header_end.saturating_add(4);
    let mut body = bytes.get(body_start..).unwrap_or_default().to_vec();
    if body.len() > content_length {
        body.truncate(content_length);
    }
    if body.len() < content_length {
        let missing = content_length - body.len();
        let start = body.len();
        body.resize(content_length, 0);
        stream
            .read_exact(body.get_mut(start..).unwrap_or_default())
            .await?;
        debug_assert_eq!(missing, content_length - start);
    }
    Ok(ParsedRequest {
        method,
        target,
        path,
        headers,
        body,
    })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn write_error(stream: &mut TcpStream, status: u16, reason: &str) -> Result<()> {
    let body = format!("{status} {reason}\n");
    stream
        .write_all(
            format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    Ok(())
}

fn write_event(
    state: &ProxyState,
    request: &ParsedRequest,
    status: u16,
    duration: Duration,
) -> Result<()> {
    let timestamp_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let event = ProxyEvent {
        timestamp_ms,
        method: &request.method,
        path: &request.path,
        status,
        duration_ms: duration.as_millis(),
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

fn constant_time_eq(left: &str, right: &str) -> bool {
    let mut difference = left.len() ^ right.len();
    let max_len = left.len().max(right.len());
    for index in 0..max_len {
        let left = left.as_bytes().get(index).copied().unwrap_or_default();
        let right = right.as_bytes().get(index).copied().unwrap_or_default();
        difference |= usize::from(left ^ right);
    }
    difference == 0
}

fn random_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")
        .wrap_err("open operating-system random source")?
        .read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn openai_allowed_paths() -> BTreeSet<String> {
    BTreeSet::from([
        String::from("/v1/chat/completions"),
        String::from("/v1/models"),
        String::from("/v1/responses"),
    ])
}

fn codex_allowed_paths() -> BTreeSet<String> {
    BTreeSet::from([
        String::from("/backend-api/codex/responses"),
        String::from("/backend-api/models"),
    ])
}

fn tracing_fallback(message: &str) {
    let path = std::env::temp_dir().join(format!(
        "kraai-eval-proxy-errors-{}.log",
        std::process::id()
    ));
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}

#[cfg(test)]
mod tests {
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

        let root = std::env::temp_dir().join(format!("kraai-eval-proxy-{}", ulid::Ulid::new()));
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
            std::env::temp_dir().join(format!("kraai-eval-codex-proxy-{}", ulid::Ulid::new()));
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
}
