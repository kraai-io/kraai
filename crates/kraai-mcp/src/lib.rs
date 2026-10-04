#![forbid(unsafe_code)]

mod auth;
mod cancellation;
mod catalog;
mod config;
mod server;

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::{StreamExt, stream};
use kraai_types::McpRequest;
use serde_json::{Value, json};

pub use auth::{McpAuthState, McpAuthStatus};
pub use catalog::McpPrompt;
use catalog::ToolDefinition;
pub use config::{McpConfig, OAuthConfig, ServerConfig, TransportConfig};
use server::Server;

#[async_trait::async_trait]
pub trait McpHost: Send + Sync {
    async fn execute(&self, request: McpRequest) -> Result<Value, String>;
}

pub struct McpManager {
    servers: BTreeMap<String, Arc<Server>>,
    prompt_max_bytes: usize,
    auth_events: tokio::sync::broadcast::Sender<McpAuthStatus>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self {
            servers: BTreeMap::new(),
            prompt_max_bytes: 16384,
            auth_events: tokio::sync::broadcast::channel(64).0,
        }
    }
}

impl McpManager {
    pub fn new(config: McpConfig) -> Result<Self, String> {
        Self::build(config, None)
    }

    pub fn with_auth_storage(config: McpConfig, root: std::path::PathBuf) -> Result<Self, String> {
        Self::build(config, Some(root))
    }

    fn build(config: McpConfig, root: Option<std::path::PathBuf>) -> Result<Self, String> {
        config.validate()?;
        let auth_events = tokio::sync::broadcast::channel(64).0;
        Ok(Self {
            prompt_max_bytes: config.prompt_max_bytes,
            servers: config
                .servers
                .into_iter()
                .filter(|(_, server)| server.enabled)
                .map(|(name, config)| {
                    let server = Arc::new_cyclic(|weak| {
                        Server::new(
                            config,
                            name.clone(),
                            root.as_deref(),
                            weak.clone(),
                            auth_events.clone(),
                        )
                    });
                    (name, server)
                })
                .collect(),
            auth_events,
        })
    }

    pub fn subscribe_auth(&self) -> tokio::sync::broadcast::Receiver<McpAuthStatus> {
        self.auth_events.subscribe()
    }

    pub async fn auth_statuses(&self) -> Vec<McpAuthStatus> {
        let mut statuses = Vec::new();
        for (name, server) in &self.servers {
            statuses.push(match &server.auth {
                Some(auth) => auth.status().await,
                None => McpAuthStatus {
                    server: name.clone(),
                    sequence: 0,
                    state: McpAuthState::Unavailable,
                    error: None,
                },
            });
        }
        statuses
    }

    fn auth(&self, name: &str) -> Result<&Arc<auth::Auth>, String> {
        self.server(name)?.auth.as_ref().ok_or_else(|| {
            String::from("OAuth login is available for HTTP MCP servers without bearer_token_env")
        })
    }

    pub async fn start_login(&self, name: &str) -> Result<McpAuthStatus, String> {
        self.auth(name)?.start().await
    }

    pub async fn cancel_login(&self, name: &str) -> Result<McpAuthStatus, String> {
        self.auth(name)?.cancel(false).await
    }

    pub async fn logout(&self, name: &str) -> Result<McpAuthStatus, String> {
        self.auth(name)?.cancel(true).await
    }

    fn server(&self, name: &str) -> Result<&Server, String> {
        self.servers
            .get(name)
            .map(Arc::as_ref)
            .ok_or_else(|| format!("MCP server {name:?} is not enabled"))
    }

    fn server_list(&self) -> Value {
        json!(self.servers.iter().map(|(name, server)| json!({"server": name, "description": server.config.description})).collect::<Vec<_>>())
    }

    async fn catalog(&self) -> (Vec<ToolDefinition>, Vec<String>) {
        let results = stream::iter(self.servers.clone())
            .map(|(name, server)| async move { (name, server.tools().await) })
            .buffered(8)
            .collect::<Vec<_>>()
            .await;
        let mut definitions = Vec::new();
        let mut warnings = Vec::new();
        for (name, result) in results {
            match result {
                Ok(tools) => definitions.extend(tools.into_iter().map(|tool| ToolDefinition {
                    server: name.clone(),
                    tool,
                })),
                Err(error) => warnings.push(format!("MCP server {name}: {error}")),
            }
        }
        (definitions, warnings)
    }

    pub async fn prompt(&self) -> McpPrompt {
        let (definitions, warnings) = self.catalog().await;
        McpPrompt {
            text: catalog::render(&definitions, self.server_list(), self.prompt_max_bytes),
            warnings,
        }
    }

    pub async fn shutdown(&self) {
        stream::iter(self.servers.clone())
            .map(|(_, server)| async move { server.shutdown().await })
            .buffer_unordered(8)
            .collect::<Vec<_>>()
            .await;
    }
}

#[async_trait::async_trait]
impl McpHost for McpManager {
    async fn execute(&self, request: McpRequest) -> Result<Value, String> {
        request.validate()?;
        let result = match request {
            McpRequest::Servers => self.server_list(),
            McpRequest::Tools { server } => {
                json!(self.server(&server)?.tools().await?.into_iter().map(|tool| json!({"server": server, "name": tool.name, "description": tool.description})).collect::<Vec<_>>())
            }
            McpRequest::Describe { server, tool } => {
                let definition = self
                    .server(&server)?
                    .tools()
                    .await?
                    .into_iter()
                    .find(|candidate| candidate.name == tool)
                    .ok_or_else(|| format!("Unknown MCP tool {server}/{tool}"))?;
                json!(ToolDefinition {
                    server,
                    tool: definition
                })
            }
            McpRequest::Search { query, limit } => {
                let (definitions, warnings) = self.catalog().await;
                catalog::search(definitions, &query, limit, warnings)
            }
            McpRequest::Call {
                server,
                tool,
                arguments,
            } => self.server(&server)?.call(tool, arguments).await?,
        };
        if serde_json::to_vec(&result)
            .map_err(|error| error.to_string())?
            .len()
            > 8 * 1024 * 1024
        {
            return Err(String::from(
                "MCP response exceeds 8 MiB; the operation may have completed and was not retried",
            ));
        }
        Ok(result)
    }
}
