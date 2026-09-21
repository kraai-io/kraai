use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Context, Result, bail};
use futures::{Stream, StreamExt};
use reqwest::redirect::Policy;
use reqwest::{Client, Method};
use serde::Serialize;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::ProxyRecord;
use crate::metrics::{ProxyMetrics, UsageMetrics};

mod credentials;
mod headers;
mod request;
pub(crate) mod service;
mod telemetry;
mod usage;

use credentials::{ProxyCredentialRequest, UpstreamCredentials};
use headers::{forward_request_headers, response_head};
use request::{ParsedRequest, read_request};
use telemetry::{CacheState, write_event};
use usage::usage_from_response_body;

const PROXY_TRANSPORT_REVISION: u32 = 1;
const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024 * 1024;
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct ModelProxyRequest {
    credentials: ProxyCredentialRequest,
    pricing: crate::PricingOptions,
    max_requests: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelProxyIdentity {
    transport_revision: u32,
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
            pricing: crate::PricingOptions::default(),
            max_requests,
        }
    }

    pub fn codex_subscription(max_requests: u64) -> Self {
        Self {
            credentials: ProxyCredentialRequest::CodexSubscription,
            pricing: crate::PricingOptions::default(),
            max_requests,
        }
    }

    pub fn with_pricing(mut self, pricing: crate::PricingOptions) -> Self {
        self.pricing = pricing;
        self
    }

    pub(crate) fn pricing(&self) -> &crate::PricingOptions {
        &self.pricing
    }

    pub(crate) fn is_codex_subscription(&self) -> bool {
        matches!(self.credentials, ProxyCredentialRequest::CodexSubscription)
    }

    pub(crate) fn identity(&self) -> Result<ModelProxyIdentity> {
        let resolved = self.credentials.resolve()?;
        Ok(ModelProxyIdentity {
            transport_revision: PROXY_TRANSPORT_REVISION,
            kind: resolved.kind().to_string(),
            upstream: resolved.upstream().to_string(),
            allowed_paths: resolved.allowed_paths().into_iter().collect(),
            credential_source: resolved.credential_source(),
            credential_sha256: resolved.fingerprint(),
            max_requests: self.max_requests,
        })
    }

    pub(crate) fn start(&self, log_path: PathBuf) -> Result<ModelProxy> {
        self.start_at(
            log_path,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        )
    }

    fn start_at(&self, log_path: PathBuf, listen_address: SocketAddr) -> Result<ModelProxy> {
        let pricing = self.pricing.freeze()?;
        if self.max_requests == 0 {
            bail!("model proxy max_requests must be greater than zero");
        }
        let credentials = self.credentials.resolve()?;
        let mut proxy = ModelProxy::start(ProxyServerConfig {
            listen_address,
            upstream: credentials.upstream().to_string(),
            allowed_paths: credentials.allowed_paths(),
            kind: credentials.kind().to_string(),
            base_path: credentials.base_path().to_string(),
            credentials,
            log_path,
            max_requests: self.max_requests,
        })?;
        proxy.pricing = pricing;
        Ok(proxy)
    }
}

pub(crate) struct ModelProxy {
    address: SocketAddr,
    token: String,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    record: ProxyRecord,
    base_path: String,
    metrics: Arc<Mutex<ProxyMetrics>>,
    log_path: PathBuf,
    pricing: crate::PricingOptions,
}

impl ModelProxy {
    fn start(config: ProxyServerConfig) -> Result<Self> {
        let log_path = config.log_path.clone();
        let token = random_token()?;
        let metrics = Arc::new(Mutex::new(ProxyMetrics::default()));
        let identity_paths = config.allowed_paths.iter().cloned().collect::<Vec<_>>();
        let record = ProxyRecord {
            transport_revision: PROXY_TRANSPORT_REVISION,
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
            log_path,
            pricing: crate::PricingOptions::default(),
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

    pub(crate) fn finish(mut self) -> Result<ProxyMetrics> {
        self.shutdown_and_join();
        let mut metrics = self.metrics()?;
        match self.finish_accounting(&metrics) {
            Ok(accounting) => metrics.accounting = Some(accounting),
            Err(error) => {
                let message = format!("{error:#}");
                let path = self
                    .log_path
                    .with_file_name("request-accounting-error.json");
                let diagnostic = serde_json::json!({ "error": message });
                if let Err(write_error) = fs::write(path, diagnostic.to_string()) {
                    tracing_fallback(&write_error.to_string());
                }
                metrics.accounting_error = Some(message);
            }
        }
        Ok(metrics)
    }

    fn finish_accounting(&self, metrics: &ProxyMetrics) -> Result<crate::RequestAccounting> {
        let accounting = crate::analyze_requests(
            &self.log_path,
            metrics.requests.saturating_add(metrics.unrecorded_requests),
            &self.pricing,
        )?;
        let path = self.log_path.with_file_name("request-accounting.json");
        fs::write(&path, serde_json::to_vec_pretty(&accounting)?)
            .wrap_err_with(|| format!("failed to write {}", path.display()))?;
        Ok(accounting)
    }

    fn shutdown_and_join(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ModelProxy {
    fn drop(&mut self) {
        self.shutdown_and_join();
    }
}

struct ProxyServerConfig {
    listen_address: SocketAddr,
    upstream: String,
    credentials: UpstreamCredentials,
    allowed_paths: BTreeSet<String>,
    kind: String,
    base_path: String,
    log_path: PathBuf,
    max_requests: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DownstreamDelivery {
    Complete,
    ClientDisconnected,
}

struct ForwardOutcome {
    status: u16,
    delivery: DownstreamDelivery,
    usage: Option<UsageMetrics>,
    response_cache_state: CacheState,
}

impl ForwardOutcome {
    fn rejected(status: u16) -> Self {
        Self {
            status,
            delivery: DownstreamDelivery::Complete,
            usage: None,
            response_cache_state: CacheState::default(),
        }
    }
}

struct RelayedResponse {
    body: Vec<u8>,
    delivery: DownstreamDelivery,
}

async fn run_server(
    config: ProxyServerConfig,
    token: String,
    metrics: Arc<Mutex<ProxyMetrics>>,
    shutdown: oneshot::Receiver<()>,
    ready: mpsc::SyncSender<Result<SocketAddr>>,
) -> Result<()> {
    let listener = match TcpListener::bind(config.listen_address).await {
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
        started_requests: AtomicU64::new(0),
        metrics,
    });
    if ready.send(Ok(address)).is_err() {
        return Ok(());
    }

    serve_connections(|| listener.accept(), state, shutdown).await
}

async fn serve_connections<Accept, Accepted>(
    mut accept: Accept,
    state: Arc<ProxyState>,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<()>
where
    Accept: FnMut() -> Accepted,
    Accepted: Future<Output = io::Result<(TcpStream, SocketAddr)>>,
{
    let mut tasks = tokio::task::JoinSet::new();
    let result = loop {
        tokio::select! {
            _ = &mut shutdown => break Ok(()),
            accepted = accept() => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(color_eyre::Report::from(error)),
                };
                let state = Arc::clone(&state);
                tasks.spawn(async move {
                    if let Err(error) = handle_connection(stream, state).await {
                        tracing_fallback(&error.to_string());
                    }
                });
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    };
    let drain = async { while tasks.join_next().await.is_some() {} };
    if tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, drain)
        .await
        .is_err()
    {
        tasks.abort_all();
    }
    while tasks.join_next().await.is_some() {}
    let metrics = state
        .metrics
        .lock()
        .map_err(|error| color_eyre::eyre::eyre!("proxy metrics mutex poisoned: {error}"))
        .map(|mut metrics| {
            metrics.unrecorded_requests = state
                .started_requests
                .load(Ordering::Relaxed)
                .saturating_sub(metrics.requests);
        });
    result?;
    metrics
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
    started_requests: AtomicU64,
    metrics: Arc<Mutex<ProxyMetrics>>,
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
    state.started_requests.fetch_add(1, Ordering::Relaxed);
    let outcome = forward_request(&mut stream, &state, &request).await?;
    let duration = started.elapsed();
    record_request_metrics(&state, &outcome, duration)?;
    write_event(&state, &request, &outcome, duration)?;
    if outcome.delivery == DownstreamDelivery::Complete {
        stream.shutdown().await?;
    }
    Ok(())
}

async fn forward_request<W>(
    stream: &mut W,
    state: &ProxyState,
    request: &ParsedRequest,
) -> Result<ForwardOutcome>
where
    W: AsyncWrite + Unpin,
{
    if !state.allowed_paths.contains(&request.path) {
        write_error(stream, 404, "Not Found").await?;
        return Ok(ForwardOutcome::rejected(404));
    }
    let authorized = request
        .headers
        .iter()
        .find(|(name, _)| name == "authorization")
        .is_some_and(|(_, value)| {
            constant_time_eq(
                value.as_bytes(),
                format!("Bearer {}", state.token).as_bytes(),
            )
        });
    if !authorized {
        write_error(stream, 401, "Unauthorized").await?;
        return Ok(ForwardOutcome::rejected(401));
    }
    let method = Method::from_bytes(request.method.as_bytes())?;
    if !matches!(method, Method::GET | Method::POST) {
        write_error(stream, 405, "Method Not Allowed").await?;
        return Ok(ForwardOutcome::rejected(405));
    }
    if state.request_count.fetch_add(1, Ordering::Relaxed) >= state.max_requests {
        write_error(stream, 429, "Proxy Request Limit Exceeded").await?;
        return Ok(ForwardOutcome::rejected(429));
    }
    let response = match send_upstream(state, method, request).await {
        Ok(response) => response,
        Err(error) => {
            write_error(stream, 502, "Bad Gateway").await?;
            tracing_fallback(&format!("model proxy upstream request failed: {error}"));
            return Ok(ForwardOutcome::rejected(502));
        }
    };
    let status = response.status();
    let response_cache_state = CacheState::from_response(response.headers());
    let mut delivery = DownstreamDelivery::Complete;
    write_downstream(
        stream,
        &mut delivery,
        &response_head(status, response.headers()),
    )
    .await?;
    let relayed = relay_response(stream, delivery, response.bytes_stream()).await?;
    let usage = record_usage_metrics(state, &relayed.body)?;
    Ok(ForwardOutcome {
        status: status.as_u16(),
        delivery: relayed.delivery,
        usage,
        response_cache_state,
    })
}

async fn relay_response<W, S, B, E>(
    stream: &mut W,
    mut delivery: DownstreamDelivery,
    body: S,
) -> Result<RelayedResponse>
where
    W: AsyncWrite + Unpin,
    S: Stream<Item = std::result::Result<B, E>>,
    B: AsRef<[u8]>,
    E: Into<color_eyre::Report>,
{
    futures::pin_mut!(body);
    let mut response_bytes = 0_usize;
    let mut captured_body = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(Into::into)?;
        let chunk = chunk.as_ref();
        response_bytes = response_bytes.saturating_add(chunk.len());
        if response_bytes > MAX_RESPONSE_BODY_BYTES {
            bail!("model proxy response body exceeds limit");
        }
        captured_body.extend_from_slice(chunk);
        write_downstream(
            stream,
            &mut delivery,
            format!("{:x}\r\n", chunk.len()).as_bytes(),
        )
        .await?;
        write_downstream(stream, &mut delivery, chunk).await?;
        write_downstream(stream, &mut delivery, b"\r\n").await?;
    }
    write_downstream(stream, &mut delivery, b"0\r\n\r\n").await?;
    Ok(RelayedResponse {
        body: captured_body,
        delivery,
    })
}

fn record_usage_metrics(state: &ProxyState, body: &[u8]) -> Result<Option<UsageMetrics>> {
    let Some(usage) = usage_from_response_body(body) else {
        return Ok(None);
    };
    let mut metrics = state
        .metrics
        .lock()
        .map_err(|error| color_eyre::eyre::eyre!("proxy metrics mutex poisoned: {error}"))?;
    metrics.usage.accumulate(&usage);
    drop(metrics);
    Ok(Some(usage))
}

async fn write_downstream<W>(
    stream: &mut W,
    delivery: &mut DownstreamDelivery,
    bytes: &[u8],
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if *delivery == DownstreamDelivery::ClientDisconnected {
        return Ok(());
    }
    match stream.write_all(bytes).await {
        Ok(()) => Ok(()),
        Err(error) if is_client_disconnect(&error) => {
            *delivery = DownstreamDelivery::ClientDisconnected;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn is_client_disconnect(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
    )
}

fn record_request_metrics(
    state: &ProxyState,
    outcome: &ForwardOutcome,
    duration: Duration,
) -> Result<()> {
    let mut metrics = state
        .metrics
        .lock()
        .map_err(|error| color_eyre::eyre::eyre!("proxy metrics mutex poisoned: {error}"))?;
    metrics.requests = metrics.requests.saturating_add(1);
    if (200..400).contains(&outcome.status) {
        metrics.successful_requests = metrics.successful_requests.saturating_add(1);
    } else {
        metrics.failed_requests = metrics.failed_requests.saturating_add(1);
    }
    if outcome.delivery == DownstreamDelivery::ClientDisconnected {
        metrics.client_disconnects = metrics.client_disconnects.saturating_add(1);
    }
    metrics.duration_ms = metrics.duration_ms.saturating_add(duration.as_millis());
    drop(metrics);
    Ok(())
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
    let builder = state.client.request(
        method,
        format!("{}{}", state.upstream.trim_end_matches('/'), request.target),
    );
    forward_request_headers(
        builder,
        request,
        matches!(state.credentials, UpstreamCredentials::Codex { .. }),
    )
    .body(request.body.clone())
}

async fn write_error<W>(stream: &mut W, status: u16, reason: &str) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
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

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let max_len = left.len().max(right.len());
    for index in 0..max_len {
        let left = left.get(index).copied().unwrap_or_default();
        let right = right.get(index).copied().unwrap_or_default();
        difference |= usize::from(left ^ right);
    }
    difference == 0
}

fn random_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")
        .wrap_err("open operating-system random source")?
        .read_exact(&mut bytes)?;
    Ok(crate::cache::encode_hex(&bytes))
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
mod tests;
