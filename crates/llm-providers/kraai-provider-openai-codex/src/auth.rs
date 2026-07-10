use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use kraai_provider_core::build_finite_http_client;
use rand::Rng;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;

const AUTH_ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_CALLBACK_PORT: u16 = 1455;
const REGISTERED_FALLBACK_CALLBACK_PORT: u16 = 1457;
const DEFAULT_ORIGINATOR: &str = "codex_cli_rs";
const TOKEN_REFRESH_INTERVAL_SECS: u64 = 8 * 24 * 60 * 60;
const TOKEN_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
const DEVICE_CODE_TIMEOUT_SECS: u64 = 15 * 60;
const CALLBACK_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CALLBACK_HEADER_BYTES: usize = 16 * 1024;
const SIGN_IN_REQUIRED_MESSAGE: &str = "OpenAI sign-in required. Use /providers.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingBrowserLogin {
    pub auth_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingDeviceCodeLogin {
    pub verification_url: String,
    pub user_code: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenAiCodexLoginState {
    SignedOut,
    BrowserPending(PendingBrowserLogin),
    DeviceCodePending(PendingDeviceCodeLogin),
    Authenticated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAiCodexAuthStatus {
    pub state: OpenAiCodexLoginState,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub account_id: Option<String>,
    pub last_refresh_unix: Option<u64>,
    pub error: Option<String>,
}

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
pub(crate) struct RequestAuth {
    pub(crate) access_token: String,
    pub(crate) account_id: String,
    generation: String,
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

impl AuthConfig {
    fn default() -> io::Result<Self> {
        Ok(Self {
            issuer: AUTH_ISSUER.to_string(),
            client_id: CLIENT_ID.to_string(),
            default_callback_port: DEFAULT_CALLBACK_PORT,
            fallback_callback_ports: vec![REGISTERED_FALLBACK_CALLBACK_PORT],
            auth_path: auth_path()?,
            refresh_timeout: TOKEN_REFRESH_TIMEOUT,
        })
    }
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
    task: JoinHandle<()>,
}

#[derive(Clone, Debug)]
struct StoredAuth {
    tokens: StoredTokens,
    claims: IdTokenClaims,
    last_refresh_unix: u64,
    generation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredAuthFile {
    auth_mode: String,
    tokens: StoredTokens,
    last_refresh: u64,
    #[serde(default)]
    generation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredTokens {
    id_token: String,
    access_token: String,
    refresh_token: String,
    account_id: String,
}

#[derive(Clone, Debug, Default)]
struct IdTokenClaims {
    email: Option<String>,
    plan_type: Option<String>,
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct RootClaims {
    #[serde(default)]
    email: Option<String>,
    #[serde(rename = "https://api.openai.com/profile", default)]
    profile: Option<ProfileClaims>,
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: Option<AuthClaims>,
}

#[derive(Deserialize)]
struct ProfileClaims {
    #[serde(default)]
    email: Option<String>,
}

#[derive(Deserialize)]
struct AuthClaims {
    #[serde(default)]
    chatgpt_plan_type: Option<serde_json::Value>,
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    #[serde(alias = "user_code", alias = "usercode")]
    user_code: String,
    interval: serde_json::Value,
}

#[derive(Serialize)]
struct DeviceCodeRequest<'a> {
    client_id: &'a str,
}

#[derive(Serialize)]
struct DeviceCodePollRequest<'a> {
    device_auth_id: &'a str,
    user_code: &'a str,
}

#[derive(Deserialize)]
struct DeviceCodePollSuccess {
    authorization_code: String,
    code_challenge: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
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

#[derive(Clone)]
struct PkceCodes {
    code_verifier: String,
    code_challenge: String,
}

impl OpenAiCodexAuthController {
    pub fn new() -> io::Result<Self> {
        Self::with_config(AuthConfig::default()?)
    }

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

        let controller = self.clone();
        self.install_login_task(
            OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin { auth_url }),
            async move {
                controller
                    .run_browser_login(listener, redirect_uri, pkce, state)
                    .await
            },
        )
        .await;

        self.emit_status().await
    }

    pub async fn start_device_code_login(&self) -> io::Result<OpenAiCodexAuthStatus> {
        let _login_guard = self.inner.login_gate.lock().await;
        self.cancel_pending_task_locked().await;

        let device_code = self.request_device_code().await?;
        let verification_url = format!("{}/codex/device", self.inner.config.issuer);

        let worker = self.clone();
        let device_auth_id = device_code.device_auth_id.clone();
        let user_code = device_code.user_code.clone();
        let interval_seconds = device_code.interval_seconds;
        self.install_login_task(
            OpenAiCodexLoginState::DeviceCodePending(PendingDeviceCodeLogin {
                verification_url: format!("{}/codex/device", self.inner.config.issuer),
                user_code: device_code.user_code,
            }),
            async move {
                worker
                    .run_device_code_login(
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

    pub(crate) async fn get_request_auth(&self) -> io::Result<RequestAuth> {
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

    pub(crate) async fn refresh_request_auth(
        &self,
        expected_auth: &RequestAuth,
    ) -> io::Result<RequestAuth> {
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
        let controller = self.clone();
        let (start_tx, start_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            controller
                .finish_login_attempt(worker_id, worker.await)
                .await;
        });

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

        let result = match result {
            Ok(auth) => match acquire_auth_file_lock(self.inner.config.auth_path.clone()).await {
                Ok(_file_lock) => persist_auth_file(&self.inner.config.auth_path, &auth)
                    .map(|()| auth)
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error.to_string()),
        };

        let mut guard = self.inner.state.lock().await;
        if guard
            .pending
            .as_ref()
            .is_none_or(|pending| pending.id != id)
        {
            return;
        }
        guard.pending = None;
        match result {
            Ok(auth) => {
                guard.auth = Some(auth);
                guard.error = None;
            }
            Err(error) => guard.error = Some(error),
        }
        drop(guard);
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

    async fn run_browser_login(
        &self,
        listener: TcpListener,
        redirect_uri: String,
        pkce: PkceCodes,
        expected_state: String,
    ) -> io::Result<StoredAuth> {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let request = match read_http_request(&mut stream).await {
                Ok(request) => request,
                Err(error) => {
                    let _ = write_http_response(
                        &mut stream,
                        "400 Bad Request",
                        &format!("Invalid callback request: {error}"),
                    )
                    .await;
                    continue;
                }
            };
            let request_line = request.lines().next().unwrap_or_default().to_string();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .to_string();

            if path == "/cancel" {
                write_http_response(&mut stream, "200 OK", "Login cancelled").await?;
                return Err(io::Error::other("Login cancelled"));
            }

            let url = match url::Url::parse(&format!("http://localhost{path}")) {
                Ok(url) => url,
                Err(error) => {
                    write_http_response(&mut stream, "400 Bad Request", "Invalid callback URL")
                        .await?;
                    return Err(io::Error::other(error));
                }
            };

            if url.path() != "/auth/callback" {
                write_http_response(
                    &mut stream,
                    "404 Not Found",
                    "Waiting for OpenAI sign-in callback on /auth/callback",
                )
                .await?;
                continue;
            }

            let state = url
                .query_pairs()
                .find(|(key, _)| key == "state")
                .map(|(_, value)| value.to_string());
            let code = url
                .query_pairs()
                .find(|(key, _)| key == "code")
                .map(|(_, value)| value.to_string());
            let error = url
                .query_pairs()
                .find(|(key, _)| key == "error")
                .map(|(_, value)| value.to_string());

            if state.as_deref() != Some(expected_state.as_str()) {
                write_http_response(
                    &mut stream,
                    "400 Bad Request",
                    "OpenAI sign-in failed. State mismatch.",
                )
                .await?;
                return Err(io::Error::other("OpenAI sign-in state mismatch"));
            }

            if let Some(error) = error {
                write_http_response(
                    &mut stream,
                    "400 Bad Request",
                    "OpenAI sign-in failed. You can return to Kraai.",
                )
                .await?;
                return Err(io::Error::other(error));
            }

            let Some(code) = code else {
                write_http_response(
                    &mut stream,
                    "400 Bad Request",
                    "OpenAI sign-in failed. Missing authorization code.",
                )
                .await?;
                return Err(io::Error::other("Missing OAuth code"));
            };
            let auth = self
                .exchange_authorization_code(&redirect_uri, &pkce, &code)
                .await?;
            write_http_response(
                &mut stream,
                "200 OK",
                "OpenAI sign-in complete. You can return to Kraai.",
            )
            .await?;
            return Ok(auth);
        }
    }

    async fn request_device_code(&self) -> io::Result<DeviceCodeResponseData> {
        let response = self
            .inner
            .client
            .post(format!(
                "{}/api/accounts/deviceauth/usercode",
                self.inner.config.issuer
            ))
            .json(&DeviceCodeRequest {
                client_id: &self.inner.config.client_id,
            })
            .send()
            .await
            .map_err(io::Error::other)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(io::Error::other(format!(
                "OpenAI device-code start failed: {status}: {body}"
            )));
        }

        let response = response
            .json::<DeviceCodeResponse>()
            .await
            .map_err(io::Error::other)?;
        Ok(DeviceCodeResponseData {
            device_auth_id: response.device_auth_id,
            user_code: response.user_code,
            interval_seconds: parse_interval_seconds(&response.interval),
        })
    }

    async fn run_device_code_login(
        &self,
        device_auth_id: String,
        user_code: String,
        interval_seconds: u64,
        verification_url: String,
    ) -> io::Result<StoredAuth> {
        tokio::time::timeout(
            Duration::from_secs(DEVICE_CODE_TIMEOUT_SECS),
            self.run_device_code_login_inner(
                device_auth_id,
                user_code,
                interval_seconds,
                verification_url,
            ),
        )
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "OpenAI device-code login timed out",
            )
        })?
    }

    async fn run_device_code_login_inner(
        &self,
        device_auth_id: String,
        user_code: String,
        interval_seconds: u64,
        verification_url: String,
    ) -> io::Result<StoredAuth> {
        let interval = Duration::from_secs(interval_seconds.clamp(1, 30));
        loop {
            let response = self
                .inner
                .client
                .post(format!(
                    "{}/api/accounts/deviceauth/token",
                    self.inner.config.issuer
                ))
                .json(&DeviceCodePollRequest {
                    device_auth_id: &device_auth_id,
                    user_code: &user_code,
                })
                .send()
                .await
                .map_err(io::Error::other)?;

            let status = response.status();
            if status.is_success() {
                let response = response
                    .json::<DeviceCodePollSuccess>()
                    .await
                    .map_err(io::Error::other)?;
                let auth = self
                    .exchange_authorization_code(
                        &format!("{}/deviceauth/callback", self.inner.config.issuer),
                        &PkceCodes {
                            code_verifier: response.code_verifier,
                            code_challenge: response.code_challenge,
                        },
                        &response.authorization_code,
                    )
                    .await?;
                return Ok(auth);
            }

            if status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND {
                tokio::time::sleep(interval).await;
                continue;
            }

            let body = response.text().await.unwrap_or_default();
            return Err(io::Error::other(format!(
                "OpenAI device-code poll failed: {status}: {body} ({verification_url})"
            )));
        }
    }

    async fn exchange_authorization_code(
        &self,
        redirect_uri: &str,
        pkce: &PkceCodes,
        code: &str,
    ) -> io::Result<StoredAuth> {
        let response = self
            .inner
            .client
            .post(format!("{}/oauth/token", self.inner.config.issuer))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
                urlencoding::encode(code),
                urlencoding::encode(redirect_uri),
                urlencoding::encode(&self.inner.config.client_id),
                urlencoding::encode(&pkce.code_verifier)
            ))
            .send()
            .await
            .map_err(io::Error::other)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(io::Error::other(format!(
                "OpenAI OAuth token exchange failed: {status}: {body}"
            )));
        }

        let tokens = response
            .json::<OAuthTokenResponse>()
            .await
            .map_err(io::Error::other)?;
        let claims = parse_id_token_claims(&tokens.id_token)?;
        let account_id = claims
            .account_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| io::Error::other("Missing ChatGPT account id in OpenAI auth token"))?;

        Ok(StoredAuth {
            tokens: StoredTokens {
                id_token: tokens.id_token,
                access_token: tokens.access_token,
                refresh_token: tokens.refresh_token,
                account_id,
            },
            claims,
            last_refresh_unix: unix_now(),
            generation: generate_generation(),
        })
    }
}

struct DeviceCodeResponseData {
    device_auth_id: String,
    user_code: String,
    interval_seconds: u64,
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

fn parse_id_token_claims(token: &str) -> io::Result<IdTokenClaims> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| io::Error::other("Invalid OpenAI id_token"))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(io::Error::other)?;
    let claims = serde_json::from_slice::<RootClaims>(&bytes).map_err(io::Error::other)?;
    let email = claims
        .email
        .or_else(|| claims.profile.and_then(|profile| profile.email));
    let (plan_type, account_id) = match claims.auth {
        Some(auth) => (
            auth.chatgpt_plan_type
                .map(|value| match value {
                    serde_json::Value::String(text) => text,
                    other => other.to_string(),
                })
                .map(|text| normalize_plan_type(&text)),
            auth.chatgpt_account_id,
        ),
        None => (None, None),
    };

    Ok(IdTokenClaims {
        email,
        plan_type,
        account_id,
    })
}

fn normalize_plan_type(plan_type: &str) -> String {
    match plan_type.to_ascii_lowercase().as_str() {
        "free" => "Free".to_string(),
        "go" => "Go".to_string(),
        "plus" => "Plus".to_string(),
        "pro" => "Pro".to_string(),
        "team" => "Team".to_string(),
        "business" => "Business".to_string(),
        "enterprise" => "Enterprise".to_string(),
        "education" | "edu" => "Edu".to_string(),
        _ => plan_type.to_string(),
    }
}

fn auth_path() -> io::Result<PathBuf> {
    let home = directories::BaseDirs::new()
        .ok_or_else(|| io::Error::other("Failed to locate home directory"))?
        .home_dir()
        .to_path_buf();
    Ok(home.join(".kraai/provider-state/openai-codex/auth.json"))
}

fn load_auth_file(path: &Path) -> io::Result<Option<StoredAuth>> {
    if !path.exists() {
        return Ok(None);
    }

    let file = std::fs::read(path)?;
    let stored = serde_json::from_slice::<StoredAuthFile>(&file).map_err(io::Error::other)?;
    let claims = parse_id_token_claims(&stored.tokens.id_token)?;
    let generation = if stored.generation.is_empty() {
        token_generation(&stored.tokens.refresh_token)
    } else {
        stored.generation
    };
    Ok(Some(StoredAuth {
        tokens: stored.tokens,
        claims,
        last_refresh_unix: stored.last_refresh,
        generation,
    }))
}

fn persist_auth_file(path: &Path, auth: &StoredAuth) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let payload = serde_json::to_vec_pretty(&StoredAuthFile {
        auth_mode: "chatgpt".to_string(),
        tokens: auth.tokens.clone(),
        last_refresh: auth.last_refresh_unix,
        generation: auth.generation.clone(),
    })
    .map_err(io::Error::other)?;
    let temp_path = temp_auth_write_path(path);
    std::fs::write(&temp_path, payload)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&temp_path, permissions)?;
    }

    rename_auth_file(&temp_path, path)?;

    Ok(())
}

fn temp_auth_write_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "auth.json".to_string());
    let mut random_bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut random_bytes);
    let suffix = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes);
    path.with_file_name(format!(".{file_name}.{suffix}.tmp"))
}

#[cfg(not(windows))]
fn rename_auth_file(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn rename_auth_file(source: &Path, destination: &Path) -> io::Result<()> {
    if destination.exists() {
        std::fs::remove_file(destination)?;
    }
    std::fs::rename(source, destination)
}

fn delete_auth_file(path: &Path) -> io::Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn request_auth(auth: &StoredAuth) -> RequestAuth {
    RequestAuth {
        access_token: auth.tokens.access_token.clone(),
        account_id: auth.tokens.account_id.clone(),
        generation: auth.generation.clone(),
    }
}

fn token_generation(refresh_token: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(refresh_token.as_bytes()))
}

fn generate_generation() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

struct AuthFileLock {
    _file: File,
}

impl AuthFileLock {
    fn acquire(auth_path: &Path) -> io::Result<Self> {
        if let Some(parent) = auth_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = auth_path.with_extension("json.refresh.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        file.lock()?;
        Ok(Self { _file: file })
    }
}

async fn acquire_auth_file_lock(auth_path: PathBuf) -> io::Result<AuthFileLock> {
    tokio::task::spawn_blocking(move || AuthFileLock::acquire(&auth_path))
        .await
        .map_err(io::Error::other)?
}

async fn bind_listener(port: u16, fallback_ports: &[u16]) -> io::Result<TcpListener> {
    let mut attempted = Vec::with_capacity(fallback_ports.len() + 1);
    for candidate in std::iter::once(port).chain(fallback_ports.iter().copied()) {
        if candidate == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "OAuth callback ports must be explicitly registered, not port 0",
            ));
        }
        if attempted.contains(&candidate) {
            continue;
        }
        attempted.push(candidate);

        match TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], candidate))).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AddrInUse,
        format!(
            "all registered OAuth callback ports are in use: {}",
            attempted
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ))
}

fn build_authorize_url(
    issuer: &str,
    client_id: &str,
    redirect_uri: &str,
    pkce: &PkceCodes,
    state: &str,
) -> String {
    let query = [
        ("response_type", "code".to_string()),
        ("client_id", client_id.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        (
            "scope",
            "openid profile email offline_access api.connectors.read api.connectors.invoke"
                .to_string(),
        ),
        ("code_challenge", pkce.code_challenge.clone()),
        ("code_challenge_method", "S256".to_string()),
        ("id_token_add_organizations", "true".to_string()),
        ("codex_cli_simplified_flow", "true".to_string()),
        ("state", state.to_string()),
        ("originator", DEFAULT_ORIGINATOR.to_string()),
    ];
    let encoded = query
        .iter()
        .map(|(key, value)| format!("{key}={}", urlencoding::encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{issuer}/oauth/authorize?{encoded}")
}

fn generate_pkce() -> PkceCodes {
    let mut bytes = [0u8; 64];
    rand::rng().fill_bytes(&mut bytes);
    let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    PkceCodes {
        code_verifier,
        code_challenge,
    }
}

fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> io::Result<String> {
    tokio::time::timeout(CALLBACK_REQUEST_TIMEOUT, async {
        let mut request = Vec::with_capacity(1024);
        loop {
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                return String::from_utf8(request).map_err(io::Error::other);
            }
            if request.len() >= MAX_CALLBACK_HEADER_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("callback request headers exceed {MAX_CALLBACK_HEADER_BYTES} bytes"),
                ));
            }

            let remaining = MAX_CALLBACK_HEADER_BYTES - request.len();
            let mut buffer = [0_u8; 1024];
            let read_capacity = remaining.min(buffer.len());
            let size = stream.read(&mut buffer[..read_capacity]).await?;
            if size == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "callback connection closed before HTTP headers completed",
                ));
            }
            request.extend_from_slice(&buffer[..size]);
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "callback request timed out"))?
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    message: &str,
) -> io::Result<()> {
    let body =
        format!("<html><body><pre style=\"font-family: monospace\">{message}</pre></body></html>");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

fn parse_interval_seconds(value: &serde_json::Value) -> u64 {
    match value {
        serde_json::Value::String(text) => text.trim().parse::<u64>().unwrap_or(5),
        serde_json::Value::Number(number) => number.as_u64().unwrap_or(5),
        _ => 5,
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
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Barrier, oneshot};
    use tokio::task::AbortHandle;
    use ulid::Ulid;

    fn is_missing_system_ca_error(error: &dyn std::error::Error) -> bool {
        let mut current = Some(error);
        while let Some(error) = current {
            let display = error.to_string();
            let debug = format!("{error:?}");
            if display.contains("No CA certificates were loaded from the system")
                || debug.contains("No CA certificates were loaded from the system")
                || display == "builder error"
            {
                return true;
            }
            current = error.source();
        }
        false
    }

    fn auth_controller_or_skip() -> Option<OpenAiCodexAuthController> {
        match OpenAiCodexAuthController::new_with_options(OpenAiCodexAuthControllerOptions::new(
            temp_auth_path(),
        )) {
            Ok(controller) => Some(controller),
            Err(error) if is_missing_system_ca_error(&error) => None,
            Err(error) => panic!("unexpected auth controller init error: {error}"),
        }
    }

    fn auth_controller_with_issuer_or_skip(
        auth_path: PathBuf,
        issuer: String,
    ) -> Option<OpenAiCodexAuthController> {
        let mut options = OpenAiCodexAuthControllerOptions::new(auth_path);
        options.issuer = issuer;
        match OpenAiCodexAuthController::new_with_options(options) {
            Ok(controller) => Some(controller),
            Err(error) if is_missing_system_ca_error(&error) => None,
            Err(error) => panic!("unexpected auth controller init error: {error}"),
        }
    }

    fn auth_controller_with_refresh_timeout_or_skip(
        auth_path: PathBuf,
        issuer: String,
        refresh_timeout: Duration,
    ) -> Option<OpenAiCodexAuthController> {
        let mut options = OpenAiCodexAuthControllerOptions::new(auth_path);
        options.issuer = issuer;
        let mut config = AuthConfig::from(options);
        config.refresh_timeout = refresh_timeout;
        match OpenAiCodexAuthController::with_config(config) {
            Ok(controller) => Some(controller),
            Err(error) if is_missing_system_ca_error(&error) => None,
            Err(error) => panic!("unexpected auth controller init error: {error}"),
        }
    }

    fn temp_auth_path() -> PathBuf {
        std::env::temp_dir()
            .join(format!("agent-openai-codex-{}", Ulid::new()))
            .join("auth.json")
    }

    fn fake_jwt(email: &str, plan_type: &str, account_id: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "email": email,
                "https://api.openai.com/auth": {
                    "chatgpt_plan_type": plan_type,
                    "chatgpt_account_id": account_id
                }
            })
            .to_string(),
        );
        format!("{header}.{payload}.signature")
    }

    #[test]
    fn auth_path_uses_agent_provider_state_root() {
        let path = auth_path().unwrap();
        assert!(path.ends_with(".kraai/provider-state/openai-codex/auth.json"));
    }

    #[tokio::test]
    async fn callback_listener_prefers_primary_registered_port() {
        let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let primary = reservation.local_addr().unwrap().port();
        drop(reservation);

        let listener = bind_listener(primary, &[]).await.unwrap();

        assert_eq!(listener.local_addr().unwrap().port(), primary);
    }

    #[tokio::test]
    async fn callback_listener_uses_registered_fallback() {
        let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let primary = primary_listener.local_addr().unwrap().port();
        let fallback_reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fallback = fallback_reservation.local_addr().unwrap().port();
        drop(fallback_reservation);

        let listener = bind_listener(primary, &[fallback]).await.unwrap();

        assert_eq!(listener.local_addr().unwrap().port(), fallback);
    }

    #[tokio::test]
    async fn callback_listener_fails_when_registered_ports_are_occupied() {
        let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let primary = primary_listener.local_addr().unwrap().port();
        let fallback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fallback = fallback_listener.local_addr().unwrap().port();

        let error = bind_listener(primary, &[fallback]).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert!(error.to_string().contains(&primary.to_string()));
        assert!(error.to_string().contains(&fallback.to_string()));
    }

    #[tokio::test]
    async fn callback_request_reader_accepts_fragmented_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let writer = tokio::spawn(async move {
            let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
            for chunk in [
                b"GET /auth/call".as_slice(),
                b"back?code=test&state=ok HTTP/1.1\r\nHost: local".as_slice(),
                b"host\r\n\r".as_slice(),
                b"\n".as_slice(),
            ] {
                client.write_all(chunk).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        let (mut server, _) = listener.accept().await.unwrap();

        let request = read_http_request(&mut server).await.unwrap();
        writer.await.unwrap();

        assert!(request.starts_with("GET /auth/callback?code=test&state=ok HTTP/1.1"));
        assert!(request.ends_with("\r\n\r\n"));
    }

    #[tokio::test]
    async fn callback_request_reader_rejects_oversized_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let writer = tokio::spawn(async move {
            let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
            client
                .write_all(&vec![b'x'; MAX_CALLBACK_HEADER_BYTES + 1])
                .await
                .unwrap();
        });
        let (mut server, _) = listener.accept().await.unwrap();

        let error = read_http_request(&mut server).await.unwrap_err();
        writer.await.unwrap();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn callback_request_reader_times_out_idle_connections() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = tokio::spawn(tokio::net::TcpStream::connect(address));
        let (mut server, _) = listener.accept().await.unwrap();
        let _client = client.await.unwrap().unwrap();

        let error = read_http_request(&mut server).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn immediately_failing_login_does_not_leave_pending_state() {
        let Some(controller) = auth_controller_or_skip() else {
            return;
        };
        controller
            .install_login_task(
                OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                    auth_url: String::from("https://example.invalid"),
                }),
                async { Err(io::Error::other("immediate failure")) },
            )
            .await;

        let status = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let status = controller.get_status().await;
                if status.error.is_some() {
                    break status;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        assert_eq!(status.state, OpenAiCodexLoginState::SignedOut);
        assert_eq!(status.error.as_deref(), Some("immediate failure"));
    }

    #[tokio::test]
    async fn stale_login_completion_cannot_clear_newer_attempt() {
        let Some(controller) = auth_controller_or_skip() else {
            return;
        };
        controller
            .install_login_task(
                OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                    auth_url: String::from("https://old.invalid"),
                }),
                std::future::pending(),
            )
            .await;
        let old_id = controller
            .inner
            .state
            .lock()
            .await
            .pending
            .as_ref()
            .unwrap()
            .id
            .clone();

        {
            let _login_guard = controller.inner.login_gate.lock().await;
            controller.cancel_pending_task_locked().await;
            controller
                .install_login_task(
                    OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                        auth_url: String::from("https://new.invalid"),
                    }),
                    std::future::pending(),
                )
                .await;
        }
        controller
            .finish_login_attempt(old_id, Err(io::Error::other("stale failure")))
            .await;

        let status = controller.get_status().await;
        assert_eq!(
            status.state,
            OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                auth_url: String::from("https://new.invalid"),
            })
        );
        assert!(status.error.is_none());

        let _login_guard = controller.inner.login_gate.lock().await;
        controller.cancel_pending_task_locked().await;
    }

    #[test]
    fn id_token_claims_extract_email_plan_and_account_id() {
        let claims =
            parse_id_token_claims(&fake_jwt("user@example.com", "pro", "workspace_123")).unwrap();

        assert_eq!(claims.email.as_deref(), Some("user@example.com"));
        assert_eq!(claims.plan_type.as_deref(), Some("Pro"));
        assert_eq!(claims.account_id.as_deref(), Some("workspace_123"));
    }

    #[tokio::test]
    async fn missing_auth_file_reports_signed_out_status() {
        let Some(controller) = auth_controller_or_skip() else {
            return;
        };

        assert_eq!(
            controller.get_status().await.state,
            OpenAiCodexLoginState::SignedOut
        );
    }

    #[test]
    fn browser_login_url_contains_official_parameters() {
        let pkce = generate_pkce();
        let auth_url = build_authorize_url(
            AUTH_ISSUER,
            CLIENT_ID,
            "http://localhost:1455/auth/callback",
            &pkce,
            "state",
        );

        assert!(auth_url.contains("id_token_add_organizations=true"));
        assert!(auth_url.contains("codex_cli_simplified_flow=true"));
        assert!(auth_url.contains("originator=codex_cli_rs"));
    }

    fn stored_auth(
        email: &str,
        plan_type: &str,
        account_id: &str,
        last_refresh_unix: u64,
    ) -> StoredAuth {
        StoredAuth {
            tokens: StoredTokens {
                id_token: fake_jwt(email, plan_type, account_id),
                access_token: format!("access-{account_id}"),
                refresh_token: format!("refresh-{account_id}"),
                account_id: account_id.to_string(),
            },
            claims: IdTokenClaims {
                email: Some(email.to_string()),
                plan_type: Some(normalize_plan_type(plan_type)),
                account_id: Some(account_id.to_string()),
            },
            last_refresh_unix,
            generation: generate_generation(),
        }
    }

    async fn scripted_refresh_server(account_id: &str) -> (String, Arc<AtomicUsize>, AbortHandle) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = requests.clone();
        let id_token = fake_jwt("refreshed@example.com", "pro", account_id);
        let account_id = account_id.to_string();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let requests = server_requests.clone();
                let id_token = id_token.clone();
                let account_id = account_id.clone();
                tokio::spawn(async move {
                    let mut request = [0_u8; 4096];
                    let _ = stream.read(&mut request).await.unwrap();
                    let request_number = requests.fetch_add(1, Ordering::SeqCst);
                    let (status, body) = if request_number == 0 {
                        (
                            "200 OK",
                            serde_json::json!({
                                "id_token": id_token,
                                "access_token": format!("access-refreshed-{account_id}"),
                                "refresh_token": format!("refresh-rotated-{account_id}")
                            })
                            .to_string(),
                        )
                    } else {
                        (
                            "401 Unauthorized",
                            "rotated refresh token reused".to_string(),
                        )
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                });
            }
        });
        (format!("http://{address}"), requests, task.abort_handle())
    }

    async fn stalled_refresh_server() -> (String, oneshot::Receiver<()>, AbortHandle) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_received, received) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            let _ = request_received.send(());
            std::future::pending::<()>().await;
        });
        (format!("http://{address}"), received, task.abort_handle())
    }

    async fn run_concurrent_refreshes(
        controller: OpenAiCodexAuthController,
        expected_auth: RequestAuth,
        task_count: usize,
    ) -> Vec<io::Result<RequestAuth>> {
        let barrier = Arc::new(Barrier::new(task_count));
        let tasks = (0..task_count).map(|_| {
            let controller = controller.clone();
            let expected_auth = expected_auth.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                controller.refresh_request_auth(&expected_auth).await
            })
        });
        futures::future::join_all(tasks)
            .await
            .into_iter()
            .map(|result| result.unwrap())
            .collect()
    }

    #[tokio::test]
    async fn simultaneous_proactive_refreshes_use_one_network_request() {
        let path = temp_auth_path();
        persist_auth_file(
            &path,
            &stored_auth("user@example.com", "pro", "workspace_123", 0),
        )
        .unwrap();
        let (issuer, requests, server) = scripted_refresh_server("workspace_123").await;
        let Some(controller) = auth_controller_with_issuer_or_skip(path.clone(), issuer) else {
            server.abort();
            return;
        };

        let tasks = (0..8).map(|_| {
            let controller = controller.clone();
            tokio::spawn(async move { controller.get_request_auth().await })
        });
        for result in futures::future::join_all(tasks).await {
            assert_eq!(
                result.unwrap().unwrap().access_token,
                "access-refreshed-workspace_123"
            );
        }
        assert_eq!(requests.load(Ordering::SeqCst), 1);

        server.abort();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn simultaneous_unauthorized_recovery_reuses_refreshed_credentials() {
        let path = temp_auth_path();
        persist_auth_file(
            &path,
            &stored_auth("user@example.com", "pro", "workspace_123", unix_now()),
        )
        .unwrap();
        let (issuer, requests, server) = scripted_refresh_server("workspace_123").await;
        let Some(controller) = auth_controller_with_issuer_or_skip(path.clone(), issuer) else {
            server.abort();
            return;
        };
        let expected_auth = controller.get_request_auth().await.unwrap();

        let results = run_concurrent_refreshes(controller, expected_auth, 8).await;
        for result in results {
            assert_eq!(
                result.unwrap().access_token,
                "access-refreshed-workspace_123"
            );
        }
        assert_eq!(requests.load(Ordering::SeqCst), 1);

        server.abort();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn separate_controllers_do_not_reuse_a_rotated_refresh_token() {
        let path = temp_auth_path();
        persist_auth_file(
            &path,
            &stored_auth("user@example.com", "pro", "workspace_123", unix_now()),
        )
        .unwrap();
        let (issuer, requests, server) = scripted_refresh_server("workspace_123").await;
        let Some(first) = auth_controller_with_issuer_or_skip(path.clone(), issuer.clone()) else {
            server.abort();
            return;
        };
        let Some(second) = auth_controller_with_issuer_or_skip(path.clone(), issuer) else {
            server.abort();
            return;
        };
        let mut first_updates = first.subscribe();
        let mut second_updates = second.subscribe();
        let expected_auth = first.get_request_auth().await.unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let first_task = {
            let barrier = barrier.clone();
            let expected_auth = expected_auth.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                first.refresh_request_auth(&expected_auth).await
            })
        };
        let second_task = tokio::spawn(async move {
            barrier.wait().await;
            second.refresh_request_auth(&expected_auth).await
        });

        assert_eq!(
            first_task.await.unwrap().unwrap().account_id,
            "workspace_123"
        );
        assert_eq!(
            second_task.await.unwrap().unwrap().account_id,
            "workspace_123"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            first_updates.try_recv().unwrap().account_id.as_deref(),
            Some("workspace_123")
        );
        assert_eq!(
            second_updates.try_recv().unwrap().account_id.as_deref(),
            Some("workspace_123")
        );

        server.abort();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn stalled_refresh_releases_auth_file_lock_after_timeout() {
        let path = temp_auth_path();
        persist_auth_file(
            &path,
            &stored_auth("user@example.com", "pro", "workspace_123", unix_now()),
        )
        .unwrap();
        let (issuer, request_received, server) = stalled_refresh_server().await;
        let Some(controller) = auth_controller_with_refresh_timeout_or_skip(
            path.clone(),
            issuer,
            Duration::from_millis(100),
        ) else {
            server.abort();
            return;
        };
        let expected_auth = controller.get_request_auth().await.unwrap();

        let refresh_controller = controller.clone();
        let refresh = tokio::spawn(async move {
            refresh_controller
                .refresh_request_auth(&expected_auth)
                .await
        });
        request_received.await.unwrap();
        let mut logout = tokio::spawn(async move { controller.logout().await });
        tokio::task::yield_now().await;
        assert!(!logout.is_finished());

        let refresh_result = tokio::time::timeout(Duration::from_secs(2), refresh)
            .await
            .unwrap()
            .unwrap();
        let Err(refresh_error) = refresh_result else {
            panic!("stalled refresh unexpectedly succeeded");
        };
        let request_error = refresh_error
            .get_ref()
            .and_then(|error| error.downcast_ref::<reqwest::Error>())
            .unwrap();
        assert!(request_error.is_timeout());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), &mut logout)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .state,
            OpenAiCodexLoginState::SignedOut
        );

        server.abort();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn stale_refresh_cannot_overwrite_or_clear_newer_credentials() {
        let path = temp_auth_path();
        persist_auth_file(
            &path,
            &stored_auth("old@example.com", "pro", "workspace_old", unix_now()),
        )
        .unwrap();
        let (issuer, requests, server) = scripted_refresh_server("workspace_old").await;
        let Some(controller) = auth_controller_with_issuer_or_skip(path.clone(), issuer) else {
            server.abort();
            return;
        };
        let stale_auth = controller.get_request_auth().await.unwrap();

        controller
            .replace_auth_for_test(stored_auth(
                "new@example.com",
                "team",
                "workspace_new",
                unix_now(),
            ))
            .await
            .unwrap();
        let Err(error) = controller.refresh_request_auth(&stale_auth).await else {
            panic!("stale refresh unexpectedly succeeded");
        };

        assert_eq!(
            error.to_string(),
            "OpenAI account changed during token refresh"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        let current = controller.get_request_auth().await.unwrap();
        assert_eq!(current.account_id, "workspace_new");
        assert_eq!(current.access_token, "access-workspace_new");
        assert_eq!(
            load_auth_file(&path).unwrap().unwrap().tokens.account_id,
            "workspace_new"
        );

        server.abort();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persisted_auth_file_round_trips() {
        let path = temp_auth_path();
        let auth = stored_auth("user@example.com", "pro", "workspace_123", 42);

        persist_auth_file(&path, &auth).unwrap();

        let loaded = load_auth_file(&path).unwrap().unwrap();
        assert_eq!(loaded.tokens.account_id, "workspace_123");
        assert_eq!(loaded.claims.email.as_deref(), Some("user@example.com"));
        assert_eq!(loaded.claims.plan_type.as_deref(), Some("Pro"));
        assert_eq!(loaded.last_refresh_unix, 42);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persisted_auth_file_overwrites_without_leaving_temp_files() {
        let path = temp_auth_path();

        persist_auth_file(
            &path,
            &stored_auth("first@example.com", "plus", "workspace_old", 1),
        )
        .unwrap();
        persist_auth_file(
            &path,
            &stored_auth("second@example.com", "team", "workspace_new", 2),
        )
        .unwrap();

        let loaded = load_auth_file(&path).unwrap().unwrap();
        assert_eq!(loaded.tokens.account_id, "workspace_new");
        assert_eq!(loaded.claims.email.as_deref(), Some("second@example.com"));
        assert_eq!(loaded.claims.plan_type.as_deref(), Some("Team"));
        assert_eq!(loaded.last_refresh_unix, 2);

        let temp_files = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path() != path)
            .collect::<Vec<_>>();
        assert!(temp_files.is_empty());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
