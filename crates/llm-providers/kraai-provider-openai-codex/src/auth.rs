mod login;
mod status;
mod storage;
mod token;

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kraai_provider_core::build_finite_http_client;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_util::task::AbortOnDropHandle;

pub use status::{
    OpenAiCodexAuthStatus, OpenAiCodexLoginState, PendingBrowserLogin, PendingDeviceCodeLogin,
};

use login::{bind_listener, build_authorize_url, generate_pkce, generate_state};
use storage::{acquire_auth_file_lock, delete_auth_file, load_auth_file, persist_auth_file};
use token::{StoredAuth, StoredTokens, generate_generation, parse_id_token_claims};

const AUTH_ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_CALLBACK_PORT: u16 = 1455;
const REGISTERED_FALLBACK_CALLBACK_PORT: u16 = 1457;
const DEFAULT_ORIGINATOR: &str = "codex_cli_rs";
const TOKEN_REFRESH_INTERVAL_SECS: u64 = 8 * 24 * 60 * 60;
const TOKEN_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
const SIGN_IN_REQUIRED_MESSAGE: &str = "OpenAI sign-in required. Use /providers.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAiCodexAuthControllerOptions {
    pub issuer: String,
    pub client_id: String,
    pub default_callback_port: u16,
    pub fallback_callback_ports: Vec<u16>,
    pub auth_path: PathBuf,
}

impl OpenAiCodexAuthControllerOptions {
    pub fn new(auth_path: PathBuf) -> Self {
        Self {
            issuer: AUTH_ISSUER.to_string(),
            client_id: CLIENT_ID.to_string(),
            default_callback_port: DEFAULT_CALLBACK_PORT,
            fallback_callback_ports: vec![REGISTERED_FALLBACK_CALLBACK_PORT],
            auth_path,
        }
    }
}

#[derive(Clone)]
pub struct OpenAiCodexAuthController {
    inner: std::sync::Arc<Inner>,
}

#[derive(Clone)]
pub struct OpenAiCodexRequestAuth {
    pub(crate) access_token: String,
    pub(crate) account_id: String,
    generation: String,
}

impl OpenAiCodexRequestAuth {
    pub fn apply_chatgpt_headers(&self, builder: RequestBuilder) -> RequestBuilder {
        builder
            .bearer_auth(&self.access_token)
            .header("ChatGPT-Account-Id", &self.account_id)
            .header("Origin", "https://chatgpt.com")
            .header("Referer", "https://chatgpt.com/")
            .header("User-Agent", DEFAULT_ORIGINATOR)
            .header("OpenAI-Client-Originator", DEFAULT_ORIGINATOR)
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }
}

struct Inner {
    client: Client,
    state: Mutex<ControllerState>,
    login_gate: Mutex<()>,
    refresh_gate: Mutex<()>,
    updates: broadcast::Sender<OpenAiCodexAuthStatus>,
    config: AuthConfig,
}

#[derive(Clone)]
struct AuthConfig {
    issuer: String,
    client_id: String,
    default_callback_port: u16,
    fallback_callback_ports: Vec<u16>,
    auth_path: PathBuf,
    refresh_timeout: Duration,
}

impl From<OpenAiCodexAuthControllerOptions> for AuthConfig {
    fn from(value: OpenAiCodexAuthControllerOptions) -> Self {
        Self {
            issuer: value.issuer,
            client_id: value.client_id,
            default_callback_port: value.default_callback_port,
            fallback_callback_ports: value.fallback_callback_ports,
            auth_path: value.auth_path,
            refresh_timeout: TOKEN_REFRESH_TIMEOUT,
        }
    }
}

struct ControllerState {
    auth: Option<StoredAuth>,
    pending: Option<PendingLogin>,
    error: Option<String>,
}

struct PendingLogin {
    id: String,
    state: OpenAiCodexLoginState,
    task: AbortOnDropHandle<()>,
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    client_id: &'a str,
    grant_type: &'a str,
    refresh_token: String,
}

#[derive(Deserialize)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

impl OpenAiCodexAuthController {
    pub fn new_with_options(options: OpenAiCodexAuthControllerOptions) -> io::Result<Self> {
        Self::with_config(options.into())
    }

    fn with_config(config: AuthConfig) -> io::Result<Self> {
        let client = build_finite_http_client().map_err(io::Error::other)?;
        let (updates, _) = broadcast::channel(32);
        let (auth, error) = match load_auth_file(&config.auth_path) {
            Ok(Some(auth)) => (Some(auth), None),
            Ok(None) => (None, None),
            Err(error) => (None, Some(format!("Failed to load OpenAI auth: {error}"))),
        };

        Ok(Self {
            inner: std::sync::Arc::new(Inner {
                client,
                state: Mutex::new(ControllerState {
                    auth,
                    pending: None,
                    error,
                }),
                login_gate: Mutex::new(()),
                refresh_gate: Mutex::new(()),
                updates,
                config,
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OpenAiCodexAuthStatus> {
        self.inner.updates.subscribe()
    }

    pub async fn get_status(&self) -> OpenAiCodexAuthStatus {
        self.snapshot_status().await
    }

    pub async fn status(&self) -> OpenAiCodexAuthStatus {
        self.get_status().await
    }

    pub async fn start_browser_login(&self) -> io::Result<OpenAiCodexAuthStatus> {
        let _login_guard = self.inner.login_gate.lock().await;
        self.cancel_pending_task_locked().await;

        let listener = bind_listener(
            self.inner.config.default_callback_port,
            &self.inner.config.fallback_callback_ports,
        )
        .await?;
        let actual_port = listener.local_addr()?.port();
        let redirect_uri = format!("http://localhost:{actual_port}/auth/callback");
        let pkce = generate_pkce();
        let state = generate_state();
        let auth_url = build_authorize_url(
            &self.inner.config.issuer,
            &self.inner.config.client_id,
            &redirect_uri,
            &pkce,
            &state,
        );

        let client = self.inner.client.clone();
        let config = self.inner.config.clone();
        self.install_login_task(
            OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin { auth_url }),
            async move {
                login::run_browser_login(&client, &config, listener, redirect_uri, pkce, state)
                    .await
            },
        )
        .await;

        self.emit_status().await
    }

    pub async fn start_device_code_login(&self) -> io::Result<OpenAiCodexAuthStatus> {
        let _login_guard = self.inner.login_gate.lock().await;
        self.cancel_pending_task_locked().await;

        let device_code =
            login::request_device_code(&self.inner.client, &self.inner.config).await?;
        let verification_url = format!("{}/codex/device", self.inner.config.issuer);

        let client = self.inner.client.clone();
        let config = self.inner.config.clone();
        let device_auth_id = device_code.device_auth_id.clone();
        let user_code = device_code.user_code.clone();
        let interval_seconds = device_code.interval_seconds;
        self.install_login_task(
            OpenAiCodexLoginState::DeviceCodePending(PendingDeviceCodeLogin {
                verification_url: format!("{}/codex/device", self.inner.config.issuer),
                user_code: device_code.user_code,
            }),
            async move {
                login::run_device_code_login(
                    &client,
                    &config,
                    device_auth_id,
                    user_code,
                    interval_seconds,
                    verification_url,
                )
                .await
            },
        )
        .await;

        self.emit_status().await
    }

    pub async fn cancel_login(&self) -> io::Result<OpenAiCodexAuthStatus> {
        let _login_guard = self.inner.login_gate.lock().await;
        self.cancel_pending_task_locked().await;
        {
            let mut guard = self.inner.state.lock().await;
            guard.pending = None;
            guard.error = None;
        }
        self.emit_status().await
    }

    pub async fn logout(&self) -> io::Result<OpenAiCodexAuthStatus> {
        let _login_guard = self.inner.login_gate.lock().await;
        self.cancel_pending_task_locked().await;
        let _file_lock = acquire_auth_file_lock(self.inner.config.auth_path.clone()).await?;
        {
            let mut guard = self.inner.state.lock().await;
            guard.auth = None;
            guard.pending = None;
            guard.error = None;
        }
        delete_auth_file(&self.inner.config.auth_path)?;
        self.emit_status().await
    }

    pub async fn get_request_auth(&self) -> io::Result<OpenAiCodexRequestAuth> {
        let needs_refresh = {
            let guard = self.inner.state.lock().await;
            match &guard.auth {
                Some(auth)
                    if auth.last_refresh_unix + TOKEN_REFRESH_INTERVAL_SECS <= unix_now() =>
                {
                    Some(request_auth(auth))
                }
                Some(auth) => {
                    return Ok(request_auth(auth));
                }
                None => None,
            }
        };

        if let Some(expected_auth) = needs_refresh {
            return self.refresh_request_auth(&expected_auth).await;
        }

        Err(io::Error::other(SIGN_IN_REQUIRED_MESSAGE))
    }

    pub async fn refresh_request_auth(
        &self,
        expected_auth: &OpenAiCodexRequestAuth,
    ) -> io::Result<OpenAiCodexRequestAuth> {
        let _refresh_guard = self.inner.refresh_gate.lock().await;
        let _file_lock = acquire_auth_file_lock(self.inner.config.auth_path.clone()).await?;

        let disk_auth = load_auth_file(&self.inner.config.auth_path)?;
        let (old_auth, state_changed) = {
            let mut guard = self.inner.state.lock().await;
            let state_changed = guard.auth.as_ref().map(|auth| &auth.generation)
                != disk_auth.as_ref().map(|auth| &auth.generation);
            guard.auth = disk_auth;
            if state_changed {
                guard.error = None;
            }
            (guard.auth.clone(), state_changed)
        };
        if state_changed {
            let _ = self.emit_status().await;
        }
        let old_auth = old_auth.ok_or_else(|| io::Error::other(SIGN_IN_REQUIRED_MESSAGE))?;

        if old_auth.generation != expected_auth.generation {
            if old_auth.tokens.account_id != expected_auth.account_id {
                return Err(io::Error::other(
                    "OpenAI account changed during token refresh",
                ));
            }
            return Ok(request_auth(&old_auth));
        }

        let refresh_response = self
            .inner
            .client
            .post(format!("{}/oauth/token", self.inner.config.issuer))
            .header("Content-Type", "application/json")
            .json(&RefreshRequest {
                client_id: &self.inner.config.client_id,
                grant_type: "refresh_token",
                refresh_token: old_auth.tokens.refresh_token.clone(),
            })
            .timeout(self.inner.config.refresh_timeout)
            .send()
            .await
            .map_err(io::Error::other)?;

        let status = refresh_response.status();
        if !status.is_success() {
            let body = refresh_response.text().await.unwrap_or_default();
            if status == StatusCode::UNAUTHORIZED {
                self.clear_auth_with_error_locked(String::from(
                    "OpenAI sign-in expired. Use /providers.",
                ))
                .await?;
                return Err(io::Error::other(format!(
                    "OpenAI token refresh failed: {body}"
                )));
            }
            return Err(io::Error::other(format!(
                "OpenAI token refresh failed: {status}: {body}"
            )));
        }

        let refresh = refresh_response
            .json::<RefreshResponse>()
            .await
            .map_err(io::Error::other)?;

        let id_token = refresh.id_token.unwrap_or(old_auth.tokens.id_token);
        let access_token = refresh.access_token.unwrap_or(old_auth.tokens.access_token);
        let refresh_token = refresh
            .refresh_token
            .unwrap_or(old_auth.tokens.refresh_token);
        let claims = parse_id_token_claims(&id_token)?;
        let account_id = claims
            .account_id
            .clone()
            .or_else(|| Some(old_auth.tokens.account_id.clone()))
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| io::Error::other("Missing ChatGPT account id in refreshed auth"))?;

        if expected_auth.account_id != account_id {
            self.clear_auth_with_error_locked(String::from(
                "OpenAI account changed. Use /providers.",
            ))
            .await?;
            return Err(io::Error::other(
                "OpenAI account changed during token refresh",
            ));
        }

        let stored = StoredAuth {
            tokens: StoredTokens {
                id_token,
                access_token: access_token.clone(),
                refresh_token,
                account_id: account_id.clone(),
            },
            claims,
            last_refresh_unix: unix_now(),
            generation: generate_generation(),
        };
        let request_auth = request_auth(&stored);

        persist_auth_file(&self.inner.config.auth_path, &stored)?;
        {
            let mut guard = self.inner.state.lock().await;
            guard.auth = Some(stored);
            guard.error = None;
        }
        let _ = self.emit_status().await;

        Ok(request_auth)
    }

    async fn clear_auth_with_error_locked(&self, error: String) -> io::Result<()> {
        {
            let mut guard = self.inner.state.lock().await;
            guard.auth = None;
            guard.pending = None;
            guard.error = Some(error);
        }
        delete_auth_file(&self.inner.config.auth_path)?;
        let _ = self.emit_status().await;
        Ok(())
    }

    async fn install_login_task<F>(&self, state: OpenAiCodexLoginState, worker: F)
    where
        F: Future<Output = io::Result<StoredAuth>> + Send + 'static,
    {
        let id = generate_generation();
        let worker_id = id.clone();
        let inner = std::sync::Arc::downgrade(&self.inner);
        let (start_tx, start_rx) = oneshot::channel();
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            let result = worker.await;
            if let Some(inner) = inner.upgrade() {
                OpenAiCodexAuthController { inner }
                    .finish_login_attempt(worker_id, result)
                    .await;
            }
        }));

        {
            let mut guard = self.inner.state.lock().await;
            guard.pending = Some(PendingLogin { id, state, task });
            guard.error = None;
        }
        let _ = start_tx.send(());
    }

    async fn finish_login_attempt(&self, id: String, result: io::Result<StoredAuth>) {
        let _login_guard = self.inner.login_gate.lock().await;
        let is_current = {
            let guard = self.inner.state.lock().await;
            guard
                .pending
                .as_ref()
                .is_some_and(|pending| pending.id == id)
        };
        if !is_current {
            return;
        }

        let (result, file_lock) = match result {
            Ok(auth) => match acquire_auth_file_lock(self.inner.config.auth_path.clone()).await {
                Ok(file_lock) => (Ok(auth), Some(file_lock)),
                Err(error) => (Err(error.to_string()), None),
            },
            Err(error) => (Err(error.to_string()), None),
        };

        let mut guard = self.inner.state.lock().await;
        if guard
            .pending
            .as_ref()
            .is_none_or(|pending| pending.id != id)
        {
            return;
        }
        if let Some(pending) = guard.pending.take() {
            drop(pending.task.detach());
        }
        let result = result.and_then(|auth| {
            persist_auth_file(&self.inner.config.auth_path, &auth)
                .map(|()| auth)
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(auth) => {
                guard.auth = Some(auth);
                guard.error = None;
            }
            Err(error) => guard.error = Some(error),
        }
        drop(guard);
        drop(file_lock);
        let _ = self.emit_status().await;
    }

    #[cfg(test)]
    async fn replace_auth_for_test(&self, auth: StoredAuth) -> io::Result<()> {
        let _login_guard = self.inner.login_gate.lock().await;
        self.cancel_pending_task_locked().await;
        let _file_lock = acquire_auth_file_lock(self.inner.config.auth_path.clone()).await?;
        persist_auth_file(&self.inner.config.auth_path, &auth)?;
        let mut guard = self.inner.state.lock().await;
        guard.auth = Some(auth);
        guard.error = None;
        drop(guard);
        Ok(())
    }

    async fn snapshot_status(&self) -> OpenAiCodexAuthStatus {
        let guard = self.inner.state.lock().await;
        status_from_state(&guard)
    }

    async fn emit_status(&self) -> io::Result<OpenAiCodexAuthStatus> {
        let status = self.snapshot_status().await;
        let _ = self.inner.updates.send(status.clone());
        Ok(status)
    }

    async fn cancel_pending_task_locked(&self) {
        let pending = {
            let mut guard = self.inner.state.lock().await;
            guard.pending.take()
        };
        if let Some(pending) = pending {
            pending.task.abort();
            let _ = pending.task.await;
        }
    }
}

fn status_from_state(state: &ControllerState) -> OpenAiCodexAuthStatus {
    let (login_state, email, plan_type, account_id, last_refresh_unix) =
        if let Some(pending) = &state.pending {
            (
                pending.state.clone(),
                state
                    .auth
                    .as_ref()
                    .and_then(|auth| auth.claims.email.clone()),
                state
                    .auth
                    .as_ref()
                    .and_then(|auth| auth.claims.plan_type.clone()),
                state
                    .auth
                    .as_ref()
                    .map(|auth| auth.tokens.account_id.clone())
                    .or_else(|| {
                        state
                            .auth
                            .as_ref()
                            .and_then(|auth| auth.claims.account_id.clone())
                    }),
                state.auth.as_ref().map(|auth| auth.last_refresh_unix),
            )
        } else if let Some(auth) = &state.auth {
            (
                OpenAiCodexLoginState::Authenticated,
                auth.claims.email.clone(),
                auth.claims.plan_type.clone(),
                Some(auth.tokens.account_id.clone()),
                Some(auth.last_refresh_unix),
            )
        } else {
            (OpenAiCodexLoginState::SignedOut, None, None, None, None)
        };

    OpenAiCodexAuthStatus {
        state: login_state,
        email,
        plan_type,
        account_id,
        last_refresh_unix,
        error: state.error.clone(),
    }
}

fn request_auth(auth: &StoredAuth) -> OpenAiCodexRequestAuth {
    OpenAiCodexRequestAuth {
        access_token: auth.tokens.access_token.clone(),
        account_id: auth.tokens.account_id.clone(),
        generation: auth.generation.clone(),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests use direct assertions for auth fixture setup and inspection"
)]
mod tests;
