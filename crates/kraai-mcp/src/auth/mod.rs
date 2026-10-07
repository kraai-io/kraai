mod client;
mod flow;
mod registration;
mod store;
#[cfg(test)]
mod tests;

use base64::Engine;
use std::sync::{Arc, Weak};
use std::time::Duration;

use rmcp::transport::auth::CredentialStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::{OAuthConfig, server::Server};
pub(crate) use client::OAuthClient;
use store::FileStore;

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export_to = "types.d.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpAuthState {
    Unavailable,
    SignedOut,
    Starting,
    Pending { auth_url: String },
    Authenticated,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export_to = "types.d.ts")]
pub struct McpAuthStatus {
    pub server: String,
    pub sequence: u64,
    pub state: McpAuthState,
    pub error: Option<String>,
}

struct Attempt {
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct Control {
    attempt: Option<Attempt>,
    stopped: bool,
}

pub(crate) struct Auth {
    url: String,
    config: OAuthConfig,
    root: Option<Arc<kraai_io::fs::DirectoryBootstrap>>,
    filename: String,
    server: Weak<Server>,
    control: Mutex<Control>,
    status: Mutex<McpAuthStatus>,
    events: broadcast::Sender<McpAuthStatus>,
    token_tasks: tokio_util::task::TaskTracker,
    token_gate: std::sync::Mutex<bool>,
}

impl Auth {
    pub(crate) fn new(
        name: String,
        url: String,
        config: OAuthConfig,
        root: Option<Arc<kraai_io::fs::DirectoryBootstrap>>,
        server: Weak<Server>,
        events: broadcast::Sender<McpAuthStatus>,
    ) -> Self {
        let mut hash = Sha256::new();
        hash.update(name.as_bytes());
        hash.update([0]);
        let normalized = url::Url::parse(&url).map_or_else(|_| url.clone(), |url| url.to_string());
        hash.update(normalized.as_bytes());
        hash.update([0]);
        hash.update(serde_json::to_vec(&config).unwrap_or_default());
        let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash.finalize());
        Self {
            url,
            config,
            root,
            filename: format!("{key}.json"),
            server,
            control: Mutex::default(),
            status: Mutex::new(McpAuthStatus {
                server: name,
                sequence: 0,
                state: McpAuthState::SignedOut,
                error: None,
            }),
            events,
            token_tasks: tokio_util::task::TaskTracker::new(),
            token_gate: std::sync::Mutex::new(false),
        }
    }

    fn store(&self) -> Result<FileStore, String> {
        let root = self
            .root
            .as_ref()
            .ok_or("MCP OAuth credential storage is not configured")?;
        FileStore::current(root.path().join(&self.filename), root.clone())
            .map_err(|error| error.to_string())
    }

    async fn publish(&self, state: McpAuthState, error: Option<String>) -> McpAuthStatus {
        let mut status = self.status.lock().await;
        status.sequence += 1;
        status.state = state;
        status.error = error;
        let snapshot = status.clone();
        drop(status);
        let _ = self.events.send(snapshot.clone());
        snapshot
    }

    pub(crate) async fn status(&self) -> McpAuthStatus {
        let mut status = self.status.lock().await;
        if status.sequence == 0 && self.root.is_some() {
            match self.store() {
                Ok(store) => match store.load().await {
                    Ok(Some(credentials)) if credentials.token_response.is_some() => {
                        status.state = McpAuthState::Authenticated
                    }
                    Ok(_) => {}
                    Err(error) => status.error = Some(error.to_string()),
                },
                Err(error) => status.error = Some(error),
            }
        }
        status.clone()
    }

    async fn stop_attempt(control: &mut Control) {
        if let Some(attempt) = control.attempt.take() {
            attempt.cancellation.cancel();
            let _ = attempt.task.await;
        }
    }

    pub(crate) async fn start(self: &Arc<Self>) -> Result<McpAuthStatus, String> {
        if self.root.is_none() {
            return Err(String::from(
                "MCP OAuth credential storage is not configured",
            ));
        }
        let mut control = self.control.lock().await;
        if control.stopped {
            return Err(String::from("MCP manager is shutting down"));
        }
        Self::stop_attempt(&mut control).await;
        let status = self.publish(McpAuthState::Starting, None).await;
        let cancellation = CancellationToken::new();
        let signal = cancellation.clone();
        let auth = self.clone();
        let task = tokio::spawn(async move {
            let work = async {
                let result = tokio::time::timeout(Duration::from_secs(300), auth.login())
                    .await
                    .unwrap_or_else(|_| Err(String::from("MCP login timed out")));
                if let Some(server) = auth.server.upgrade() {
                    server.invalidate().await;
                }
                match result {
                    Ok(()) => {
                        auth.publish(McpAuthState::Authenticated, None).await;
                    }
                    Err(error) => {
                        auth.publish(McpAuthState::SignedOut, Some(error)).await;
                    }
                }
            };
            tokio::select! {
                biased;
                () = signal.cancelled() => {},
                () = work => {},
            }
        });
        control.attempt = Some(Attempt { cancellation, task });
        drop(control);
        Ok(status)
    }

    async fn login(&self) -> Result<(), String> {
        let store = self
            .store()?
            .reset()
            .await
            .map_err(|error| error.to_string())?;
        if let Some(server) = self.server.upgrade() {
            server.invalidate().await;
        }
        let (session, listener) = tokio::time::timeout(
            Duration::from_secs(60),
            flow::start(&self.url, &self.config, store.clone()),
        )
        .await
        .map_err(|_timeout| String::from("MCP OAuth discovery timed out"))??;
        self.publish(
            McpAuthState::Pending {
                auth_url: session.auth_url.clone(),
            },
            None,
        )
        .await;
        flow::complete(session, listener, store).await
    }

    pub(crate) async fn cancel(&self, logout: bool) -> Result<McpAuthStatus, String> {
        let mut control = self.control.lock().await;
        let pending = control
            .attempt
            .as_ref()
            .is_some_and(|attempt| !attempt.task.is_finished());
        Self::stop_attempt(&mut control).await;
        let status = if logout || pending {
            self.store()?
                .reset()
                .await
                .map_err(|error| error.to_string())?;
            if let Some(server) = self.server.upgrade() {
                server.invalidate().await;
            }
            self.publish(McpAuthState::SignedOut, None).await
        } else {
            self.status().await
        };
        drop(control);
        Ok(status)
    }

    pub(crate) async fn shutdown(&self) {
        let mut control = self.control.lock().await;
        control.stopped = true;
        Self::stop_attempt(&mut control).await;
        if let Ok(mut stopped) = self.token_gate.lock() {
            *stopped = true;
        }
        self.token_tasks.close();
        drop(control);
        self.token_tasks.wait().await;
    }

    pub(crate) async fn client(
        self: &Arc<Self>,
        client: reqwest::Client,
    ) -> Result<Option<OAuthClient>, String> {
        if self.root.is_none() {
            return Ok(None);
        }
        let store = self.store()?;
        if store
            .load()
            .await
            .map_err(|error| error.to_string())?
            .and_then(|credentials| credentials.token_response)
            .is_none()
        {
            return Ok(None);
        }
        let _guard = store.lock().await.map_err(|error| error.to_string())?;
        let mut manager = flow::manager(&self.url, store.clone()).await?;
        if !manager
            .initialize_from_store()
            .await
            .map_err(|error| error.to_string())?
        {
            self.publish(
                McpAuthState::SignedOut,
                Some(String::from("MCP authorization changed; sign in again")),
            )
            .await;
            return Err(String::from("MCP authorization changed; sign in again"));
        }
        flow::restore_client(&mut manager, &self.config, &store).await?;
        Ok(Some(OAuthClient::new(client, manager, self.clone(), store)))
    }

    async fn required(&self, store: &FileStore, error: String) {
        let mut status = self.status.lock().await;
        if matches!(
            status.state,
            McpAuthState::Starting | McpAuthState::Pending { .. }
        ) || store.load().await.is_err()
        {
            return;
        }
        status.sequence += 1;
        status.state = McpAuthState::SignedOut;
        status.error = Some(error);
        let _ = self.events.send(status.clone());
    }
}
