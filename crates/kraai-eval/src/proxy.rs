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
use reqwest::Client;
use reqwest::redirect::Policy;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::ProxyRecord;
use crate::metrics::{ProxyMetrics, UsageMetrics};

mod credentials;
mod forwarding;
mod headers;
mod recording;
mod request;
pub(crate) mod service;
mod telemetry;
mod usage;

use credentials::{ProxyCredentialRequest, UpstreamCredentials};
use forwarding::{forward_request, rejection, write_error};
use recording::RequestRecord;
use request::{ParsedRequest, read_request_head};
use telemetry::{CacheState, write_event};
use usage::{ResponseUsage, usage_from_response_body};

const PROXY_TRANSPORT_REVISION: u32 = 1;
const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024 * 1024;
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 32;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug)]
struct ConnectionLimits {
    max_connections: usize,
    request_read_timeout: Duration,
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            max_connections: MAX_CONNECTIONS,
            request_read_timeout: REQUEST_READ_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ProxyListen {
    Tcp(SocketAddr),
    Unix {
        path: PathBuf,
        advertised: SocketAddr,
    },
}

impl ProxyListen {
    pub(crate) fn loopback() -> Self {
        Self::Tcp(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
    }
}

enum BoundListener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
}

impl BoundListener {
    async fn bind(listen: &ProxyListen) -> io::Result<(Self, SocketAddr)> {
        match listen {
            ProxyListen::Tcp(address) => {
                let listener = TcpListener::bind(address).await?;
                let address = listener.local_addr()?;
                Ok((Self::Tcp(listener), address))
            }
            #[cfg(unix)]
            ProxyListen::Unix { path, advertised } => Ok((
                Self::Unix(tokio::net::UnixListener::bind(path)?),
                *advertised,
            )),
            #[cfg(not(unix))]
            ProxyListen::Unix { path, advertised } => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "cannot serve {advertised} through {}: Unix sockets need a Unix host",
                    path.display()
                ),
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelProxyRequest {
    credentials: ProxyCredentialRequest,
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
            max_requests,
        }
    }

    pub fn codex_subscription(max_requests: u64) -> Self {
        Self {
            credentials: ProxyCredentialRequest::CodexSubscription,
            max_requests,
        }
    }

    pub fn kraai_provider(
        provider: crate::KraaiProviderConfigRequest,
        max_requests: u64,
    ) -> Result<Self> {
        Ok(match provider.kind()? {
            crate::KraaiProviderKind::Codex => Self::codex_subscription(max_requests),
            crate::KraaiProviderKind::ChatCompletions => Self {
                credentials: ProxyCredentialRequest::ChatCompletions { provider },
                max_requests,
            },
        })
    }

    pub(crate) fn kraai_provider_kind(&self) -> Option<crate::KraaiProviderKind> {
        match self.credentials {
            ProxyCredentialRequest::CodexSubscription => Some(crate::KraaiProviderKind::Codex),
            ProxyCredentialRequest::ChatCompletions { .. } => {
                Some(crate::KraaiProviderKind::ChatCompletions)
            }
            ProxyCredentialRequest::OpenAiApiKey { .. } => None,
        }
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

    pub(crate) fn start(&self, log_path: PathBuf, listen: ProxyListen) -> Result<ModelProxy> {
        if self.max_requests == 0 {
            bail!("model proxy max_requests must be greater than zero");
        }
        let credentials = self.credentials.resolve()?;
        ModelProxy::start(ProxyServerConfig {
            listen,
            upstream: credentials.upstream().to_string(),
            allowed_paths: credentials.allowed_paths(),
            kind: credentials.kind().to_string(),
            base_path: credentials.base_path().to_string(),
            credentials,
            log_path,
            max_requests: self.max_requests,
            shutdown_drain_timeout: SHUTDOWN_DRAIN_TIMEOUT,
            limits: ConnectionLimits::default(),
        })
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
                tracing_fallback(&format!("{error:#}"));
            }
        });
        let address = ready_rx.recv().wrap_err("model proxy did not start")??;
        Ok(Self {
            address,
            log_path,
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
    listen: ProxyListen,
    upstream: String,
    credentials: UpstreamCredentials,
    allowed_paths: BTreeSet<String>,
    kind: String,
    base_path: String,
    log_path: PathBuf,
    max_requests: u64,
    shutdown_drain_timeout: Duration,
    limits: ConnectionLimits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DownstreamDelivery {
    Complete,
    ClientDisconnected,
    Incomplete,
}

struct ForwardOutcome {
    status: Option<u16>,
    delivery: DownstreamDelivery,
    usage: Option<UsageMetrics>,
    reported_cost: Option<kraai_types::Usd>,
    response_cache_state: CacheState,
    upstream_request_id: Option<String>,
    error: Option<String>,
    stage: &'static str,
    body: Vec<u8>,
}

impl ForwardOutcome {
    fn pending() -> Self {
        Self {
            status: None,
            delivery: DownstreamDelivery::Incomplete,
            usage: None,
            reported_cost: None,
            response_cache_state: CacheState::default(),
            upstream_request_id: None,
            error: None,
            stage: "request_validation",
            body: Vec::new(),
        }
    }

    fn rejected(status: u16) -> Self {
        Self {
            status: Some(status),
            delivery: DownstreamDelivery::Complete,
            stage: "response_headers",
            ..Self::pending()
        }
    }
}

async fn run_server(
    config: ProxyServerConfig,
    token: String,
    metrics: Arc<Mutex<ProxyMetrics>>,
    shutdown: oneshot::Receiver<()>,
    ready: mpsc::SyncSender<Result<SocketAddr>>,
) -> Result<()> {
    let (listener, address) = match BoundListener::bind(&config.listen).await {
        Ok(bound) => bound,
        Err(error) => {
            let _ = ready.send(Err(error.into()));
            return Ok(());
        }
    };
    let log = Arc::new(Mutex::new(File::create(&config.log_path)?));
    let state = Arc::new(ProxyState {
        upstream: config.upstream,
        credentials: config.credentials,
        allowed_paths: config.allowed_paths,
        token,
        client: kraai_io::http::client_builder(
            kraai_io::http::HttpTimeouts {
                connect: Some(Duration::from_secs(10)),
                ..Default::default()
            },
            Policy::none(),
        )
        .build()?,
        log,
        max_requests: config.max_requests,
        limits: config.limits,
        request_count: AtomicU64::new(0),
        started_requests: AtomicU64::new(0),
        metrics,
    });
    if ready.send(Ok(address)).is_err() {
        return Ok(());
    }

    let drain_timeout = config.shutdown_drain_timeout;
    match &listener {
        BoundListener::Tcp(listener) => {
            serve_connections(
                || async { listener.accept().await.map(|(stream, _)| stream) },
                state,
                shutdown,
                drain_timeout,
            )
            .await
        }
        #[cfg(unix)]
        BoundListener::Unix(listener) => {
            serve_connections(
                || async { listener.accept().await.map(|(stream, _)| stream) },
                state,
                shutdown,
                drain_timeout,
            )
            .await
        }
    }
}

async fn serve_connections<Accept, Accepted, Stream>(
    mut accept: Accept,
    state: Arc<ProxyState>,
    mut shutdown: oneshot::Receiver<()>,
    drain_timeout: Duration,
) -> Result<()>
where
    Accept: FnMut() -> Accepted,
    Accepted: Future<Output = io::Result<Stream>>,
    Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut tasks = tokio::task::JoinSet::new();
    let result = loop {
        tokio::select! {
            _ = &mut shutdown => break Ok(()),
            accepted = accept(), if tasks.len() < state.limits.max_connections => {
                let stream = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(color_eyre::Report::from(error)),
                };
                let state = Arc::clone(&state);
                tasks.spawn(async move {
                    if let Err(error) = handle_connection(stream, state).await {
                        tracing_fallback(&format!("{error:#}"));
                    }
                });
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    };
    let drain = async { while tasks.join_next().await.is_some() {} };
    if tokio::time::timeout(drain_timeout, drain).await.is_err() {
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
    limits: ConnectionLimits,
    request_count: AtomicU64,
    started_requests: AtomicU64,
    metrics: Arc<Mutex<ProxyMetrics>>,
}

async fn handle_connection<Stream>(mut stream: Stream, state: Arc<ProxyState>) -> Result<()>
where
    Stream: AsyncRead + AsyncWrite + Unpin,
{
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + state.limits.request_read_timeout;
    let head = match read_before(deadline, read_request_head(&mut stream)).await {
        Ok(head) => head,
        Err(error) => return reject_unreadable(&mut stream, error).await,
    };
    if let Some((status, reason)) = rejection(&state, &head.request) {
        let mut record = RequestRecord::new(&state, &head.request, started);
        record.outcome = ForwardOutcome::rejected(status);
        record.finish(write_error(&mut stream, status, reason).await)?;
        stream.shutdown().await?;
        let _ = read_before(deadline, head.discard_body(&mut stream)).await;
        return Ok(());
    }
    let request = match read_before(deadline, head.read_body(&mut stream)).await {
        Ok(request) => request,
        Err(error) => return reject_unreadable(&mut stream, error).await,
    };
    let mut record = RequestRecord::new(&state, &request, started);
    let result = forward_request(&mut stream, &state, &request, &mut record.outcome).await;
    record.finish(result)?;
    if record.outcome.delivery == DownstreamDelivery::Complete {
        stream.shutdown().await?;
    }
    Ok(())
}

async fn read_before<T>(
    deadline: tokio::time::Instant,
    read: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout_at(deadline, read).await?
}

async fn reject_unreadable<Stream>(stream: &mut Stream, error: color_eyre::Report) -> Result<()>
where
    Stream: AsyncWrite + Unpin,
{
    if error.is::<tokio::time::error::Elapsed>() {
        write_error(stream, 408, "Request Timeout").await?;
        return Err(error.wrap_err("proxy request read deadline elapsed"));
    }
    write_error(stream, 400, "Bad Request").await?;
    Err(error)
}

fn record_usage_metrics(state: &ProxyState, body: &[u8]) -> Result<Option<ResponseUsage>> {
    let Some(usage) = usage_from_response_body(body) else {
        return Ok(None);
    };
    let mut metrics = state
        .metrics
        .lock()
        .map_err(|error| color_eyre::eyre::eyre!("proxy metrics mutex poisoned: {error}"))?;
    metrics.usage.accumulate(&usage.tokens);
    drop(metrics);
    Ok(Some(usage))
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
    if outcome.error.is_none()
        && outcome
            .status
            .is_some_and(|status| (200..400).contains(&status))
    {
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
