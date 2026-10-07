use std::collections::BTreeSet;
use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequest, CallToolRequestParams, ClientRequest, PaginatedRequestParams, ProtocolVersion,
    ServerResult, Tool,
};
use rmcp::service::{NotificationContext, PeerRequestOptions, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt, Peer, RoleClient};
use tokio::sync::Mutex;

use crate::config::{ServerConfig, TransportConfig};

mod http;

pub(crate) struct Server {
    pub(crate) config: ServerConfig,
    pub(crate) auth: Option<Arc<crate::auth::Auth>>,
    state: Mutex<State>,
    generation: Arc<AtomicU64>,
}

#[derive(Default)]
struct State {
    connection: Option<RunningService<RoleClient, Handler>>,
    tools: Vec<Tool>,
    expires: Option<Instant>,
    catalog_generation: u64,
    catalog_failure: Option<(Instant, String)>,
    failure: Option<(Instant, String)>,
}

#[derive(Clone)]
struct Handler {
    generation: Arc<AtomicU64>,
    protocol: ProtocolVersion,
}

impl ClientHandler for Handler {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    fn get_info(&self) -> rmcp::model::ClientConfig {
        rmcp::model::ClientConfig::default().with_protocol_version(self.protocol.clone())
    }
}

impl Server {
    pub(crate) fn new(
        config: ServerConfig,
        name: String,
        root: Option<&std::path::Path>,
        server: std::sync::Weak<Self>,
        events: tokio::sync::broadcast::Sender<crate::McpAuthStatus>,
    ) -> Self {
        let auth = match &config.transport {
            TransportConfig::Http {
                url,
                bearer_token_env: None,
                headers,
                oauth,
            } if !crate::headers::has_authorization(headers) => {
                Some(Arc::new(crate::auth::Auth::new(
                    name,
                    url.clone(),
                    oauth.clone().unwrap_or_default(),
                    root,
                    server,
                    events,
                )))
            }
            _ => None,
        };
        Self {
            config,
            auth,
            state: Mutex::default(),
            generation: Arc::new(AtomicU64::new(1)),
        }
    }

    async fn peer(&self) -> Result<Peer<RoleClient>, String> {
        let mut state = self.state.lock().await;
        if let Some(connection) = &state.connection
            && !connection.peer().is_transport_closed()
        {
            return Ok(connection.peer().clone());
        }
        if let Some((until, error)) = &state.failure
            && *until > Instant::now()
        {
            return Err(error.clone());
        }
        state.connection = None;
        self.generation.fetch_add(1, Ordering::AcqRel);
        let result = tokio::time::timeout(
            Duration::from_secs(self.config.startup_timeout_secs),
            self.connect(),
        )
        .await
        .map_err(|_error| String::from("MCP server startup timed out"))
        .and_then(|result| result);
        match result {
            Ok(connection) => {
                let peer = connection.peer().clone();
                state.connection = Some(connection);
                state.failure = None;
                state.catalog_failure = None;
                drop(state);
                Ok(peer)
            }
            Err(error) => {
                state.failure = Some((Instant::now() + Duration::from_secs(10), error.clone()));
                drop(state);
                Err(error)
            }
        }
    }

    async fn connect(&self) -> Result<RunningService<RoleClient, Handler>, String> {
        let handler = Handler {
            generation: self.generation.clone(),
            protocol: ProtocolVersion::LATEST,
        };
        let lifecycle = ClientLifecycleMode::Auto {
            preferred_versions: vec![rmcp::model::ProtocolVersion::LATEST],
            legacy_version: None,
        };
        match &self.config.transport {
            TransportConfig::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                let mut process = tokio::process::Command::new(command);
                process.args(args).envs(env).kill_on_drop(true);
                if let Some(cwd) = cwd {
                    process.current_dir(cwd);
                }
                let (transport, _) = TokioChildProcess::builder(process)
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|error| format!("Unable to start MCP process: {error}"))?;
                handler
                    .serve_with_lifecycle(transport, lifecycle)
                    .await
                    .map_err(|error| error.to_string())
            }
            TransportConfig::Http {
                url,
                bearer_token_env,
                headers,
                ..
            } => {
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone());
                if let Some(name) = bearer_token_env {
                    let token = std::env::var(name).map_err(|_error| {
                        format!("Missing MCP credential environment variable {name}")
                    })?;
                    if token.trim().is_empty() {
                        return Err(format!(
                            "MCP credential environment variable {name} is empty"
                        ));
                    }
                    config = config.auth_header(token);
                }
                let client = kraai_io::http::client_builder(
                    kraai_io::http::HttpTimeouts::default(),
                    reqwest::redirect::Policy::none(),
                )
                .default_headers(crate::headers::parse(headers)?)
                .build()
                .map_err(|error| error.to_string())?;
                if let Some(auth) = &self.auth
                    && let Some(client) = auth.client(client.clone()).await?
                {
                    return http::connect(client, config, handler, lifecycle).await;
                }
                http::connect(client, config, handler, lifecycle).await
            }
        }
    }

    pub(crate) async fn tools(&self) -> Result<Vec<Tool>, String> {
        let peer = self.peer().await?;
        let mut state = self.state.lock().await;
        let generation = self.generation.load(Ordering::Acquire);
        if generation == state.catalog_generation
            && state
                .expires
                .is_some_and(|expires| expires > Instant::now())
        {
            return Ok(state.tools.clone());
        }
        if let Some((until, error)) = &state.catalog_failure
            && *until > Instant::now()
        {
            return Err(error.clone());
        }
        let result = tokio::time::timeout(
            Duration::from_secs(self.config.startup_timeout_secs),
            list_tools(&peer),
        )
        .await;
        match result {
            Ok(Ok((tools, ttl))) => {
                state.tools = tools.clone();
                state.expires = Some(Instant::now() + ttl);
                state.catalog_generation = generation;
                state.catalog_failure = None;
                drop(state);
                Ok(tools)
            }
            result => {
                let error = match result {
                    Ok(Err(error)) => error,
                    _ => String::from("MCP tool discovery timed out"),
                };
                state.catalog_failure =
                    Some((Instant::now() + Duration::from_secs(10), error.clone()));
                drop(state);
                Err(error)
            }
        }
    }

    pub(crate) async fn call(
        &self,
        tool: String,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let peer = self.peer().await?;
        let request = ClientRequest::CallToolRequest(CallToolRequest::new(
            CallToolRequestParams::new(tool).with_arguments(arguments),
        ));
        let handle = peer
            .send_request_with_option(
                request,
                PeerRequestOptions::with_timeout(Duration::from_secs(
                    self.config.call_timeout_secs,
                )),
            )
            .await
            .map_err(|error| format!("MCP call could not be sent: {error}"))?;
        let mut cancellation = crate::cancellation::CancelOnDrop::new(peer, handle.id.clone());
        let response = handle.await_response().await;
        cancellation.disarm();
        let response = response.map_err(|error| {
            format!(
                "MCP call failed: {error}. The operation may have completed; it was not retried."
            )
        })?;
        match response {
            ServerResult::CallToolResult(result) => {
                serde_json::to_value(result).map_err(|error| error.to_string())
            }
            ServerResult::InputRequiredResult(_) => Err(String::from(
                "MCP tool requires an interaction that Kraai does not support",
            )),
            _ => Err(String::from(
                "Unexpected MCP tool response; the call was not retried",
            )),
        }
    }

    pub(crate) async fn shutdown(&self) {
        if let Some(auth) = &self.auth {
            auth.shutdown().await;
        }
        self.invalidate().await;
    }

    pub(crate) async fn invalidate(&self) {
        let connection = {
            let mut state = self.state.lock().await;
            let connection = state.connection.take();
            *state = State::default();
            self.generation.fetch_add(1, Ordering::AcqRel);
            drop(state);
            connection
        };
        if let Some(connection) = connection {
            connection.cancellation_token().cancel();
        }
    }
}

async fn list_tools(peer: &Peer<RoleClient>) -> Result<(Vec<Tool>, Duration), String> {
    let mut tools = Vec::new();
    let mut cursor = None;
    let mut cursors = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut ttl = Duration::from_secs(60);
    let mut bytes = 0usize;
    loop {
        let mut params = PaginatedRequestParams::default();
        params.cursor = cursor;
        let page = peer
            .list_tools(Some(params))
            .await
            .map_err(|error| error.to_string())?;
        if let Some(ms) = page.ttl_ms {
            ttl = ttl.min(Duration::from_millis(ms));
        }
        for tool in page.tools {
            if !names.insert(tool.name.to_string()) {
                return Err(format!("Duplicate MCP tool name {}", tool.name));
            }
            bytes = bytes.saturating_add(
                serde_json::to_vec(&tool)
                    .map_err(|error| error.to_string())?
                    .len(),
            );
            if bytes > 8 * 1024 * 1024 || tools.len() >= 10_000 {
                return Err(String::from("MCP tool catalog exceeds discovery limits"));
            }
            tools.push(tool);
        }
        cursor = page.next_cursor;
        let Some(next) = &cursor else {
            break;
        };
        if !cursors.insert(next.clone()) || cursors.len() > 1_000 {
            return Err(String::from("MCP server returned invalid pagination"));
        }
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((tools, ttl))
}
