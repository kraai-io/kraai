use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::Rng;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast};
use tokio::task::AbortHandle;

const AUTH_ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_CALLBACK_PORT: u16 = 1455;
const DEFAULT_ORIGINATOR: &str = "codex_cli_rs";
const TOKEN_REFRESH_INTERVAL_SECS: u64 = 8 * 24 * 60 * 60;
const DEVICE_CODE_TIMEOUT_SECS: u64 = 15 * 60;
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
    pub auth_path: PathBuf,
}

impl OpenAiCodexAuthControllerOptions {
    pub fn new(auth_path: PathBuf) -> Self {
        Self {
            issuer: AUTH_ISSUER.to_string(),
            client_id: CLIENT_ID.to_string(),
            default_callback_port: DEFAULT_CALLBACK_PORT,
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
}

struct Inner {
    client: Client,
    state: Mutex<ControllerState>,
    updates: broadcast::Sender<OpenAiCodexAuthStatus>,
    config: AuthConfig,
}

#[derive(Clone)]
struct AuthConfig {
    issuer: String,
    client_id: String,
    default_callback_port: u16,
    auth_path: PathBuf,
}

impl AuthConfig {
    fn default() -> io::Result<Self> {
        Ok(Self {
            issuer: AUTH_ISSUER.to_string(),
            client_id: CLIENT_ID.to_string(),
            default_callback_port: DEFAULT_CALLBACK_PORT,
            auth_path: auth_path()?,
        })
    }
}

impl From<OpenAiCodexAuthControllerOptions> for AuthConfig {
    fn from(value: OpenAiCodexAuthControllerOptions) -> Self {
        Self {
            issuer: value.issuer,
            client_id: value.client_id,
            default_callback_port: value.default_callback_port,
            auth_path: value.auth_path,
        }
    }
}

struct ControllerState {
    auth: Option<StoredAuth>,
    pending: Option<PendingLogin>,
    error: Option<String>,
}

struct PendingLogin {
    state: OpenAiCodexLoginState,
    abort_handle: AbortHandle,
}

#[derive(Clone, Debug)]
struct StoredAuth {
    tokens: StoredTokens,
    claims: IdTokenClaims,
    last_refresh_unix: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredAuthFile {
    auth_mode: String,
    tokens: StoredTokens,
    last_refresh: u64,
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
        let client = Client::builder().build().map_err(io::Error::other)?;
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
        self.cancel_pending_task().await;

        let listener = bind_listener(self.inner.config.default_callback_port).await?;
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
        let abort_handle = tokio::spawn(async move {
            if let Err(error) = controller
                .run_browser_login(listener, redirect_uri, pkce, state)
                .await
            {
                controller.finish_failed_login(error.to_string()).await;
            }
        })
        .abort_handle();

        {
            let mut guard = self.inner.state.lock().await;
            guard.pending = Some(PendingLogin {
                state: OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin { auth_url }),
                abort_handle,
            });
            guard.error = None;
        }

        self.emit_status().await
    }

    pub async fn start_device_code_login(&self) -> io::Result<OpenAiCodexAuthStatus> {
        self.cancel_pending_task().await;

        let device_code = self.request_device_code().await?;
        let verification_url = format!("{}/codex/device", self.inner.config.issuer);

        let controller = self.clone();
        let device_auth_id = device_code.device_auth_id.clone();
        let user_code = device_code.user_code.clone();
        let interval_seconds = device_code.interval_seconds;
        let abort_handle = tokio::spawn(async move {
            if let Err(error) = controller
                .run_device_code_login(
                    device_auth_id,
                    user_code,
                    interval_seconds,
                    verification_url,
                )
                .await
            {
                controller.finish_failed_login(error.to_string()).await;
            }
        })
        .abort_handle();

        {
            let mut guard = self.inner.state.lock().await;
            guard.pending = Some(PendingLogin {
                state: OpenAiCodexLoginState::DeviceCodePending(PendingDeviceCodeLogin {
                    verification_url: format!("{}/codex/device", self.inner.config.issuer),
                    user_code: device_code.user_code,
                }),
                abort_handle,
            });
            guard.error = None;
        }

        self.emit_status().await
    }

    pub async fn cancel_login(&self) -> io::Result<OpenAiCodexAuthStatus> {
        self.cancel_pending_task().await;
        {
            let mut guard = self.inner.state.lock().await;
            guard.pending = None;
            guard.error = None;
        }
        self.emit_status().await
    }

    pub async fn logout(&self) -> io::Result<OpenAiCodexAuthStatus> {
        self.cancel_pending_task().await;
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
                    Some(auth.tokens.account_id.clone())
                }
                Some(auth) => {
                    return Ok(RequestAuth {
                        access_token: auth.tokens.access_token.clone(),
                        account_id: auth.tokens.account_id.clone(),
                    });
                }
                None => None,
            }
        };

        if let Some(expected_account_id) = needs_refresh {
            return self.refresh_request_auth(Some(expected_account_id)).await;
        }

        Err(io::Error::other(SIGN_IN_REQUIRED_MESSAGE))
    }

    pub(crate) async fn refresh_request_auth(
        &self,
        expected_account_id: Option<String>,
    ) -> io::Result<RequestAuth> {
        let old_auth = {
            let guard = self.inner.state.lock().await;
            guard.auth.clone()
        }
        .ok_or_else(|| io::Error::other(SIGN_IN_REQUIRED_MESSAGE))?;

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
            .send()
            .await
            .map_err(io::Error::other)?;

        let status = refresh_response.status();
        if !status.is_success() {
            let body = refresh_response.text().await.unwrap_or_default();
            if status == StatusCode::UNAUTHORIZED {
                self.clear_auth_with_error(String::from("OpenAI sign-in expired. Use /providers."))
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

        if let Some(expected_account_id) = expected_account_id
            && expected_account_id != account_id
        {
            self.clear_auth_with_error(String::from("OpenAI account changed. Use /providers."))
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
        };

        persist_auth_file(&self.inner.config.auth_path, &stored)?;
        {
            let mut guard = self.inner.state.lock().await;
            guard.auth = Some(stored);
            guard.error = None;
        }
        let _ = self.emit_status().await;

        Ok(RequestAuth {
            access_token,
            account_id,
        })
    }

    async fn clear_auth_with_error(&self, error: String) -> io::Result<()> {
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

    async fn finish_failed_login(&self, error: String) {
        let mut guard = self.inner.state.lock().await;
        guard.pending = None;
        guard.error = Some(error);
        drop(guard);
        let _ = self.emit_status().await;
    }

    async fn finish_successful_login(&self, auth: StoredAuth) -> io::Result<()> {
        persist_auth_file(&self.inner.config.auth_path, &auth)?;
        {
            let mut guard = self.inner.state.lock().await;
            guard.auth = Some(auth);
            guard.pending = None;
            guard.error = None;
        }
        let _ = self.emit_status().await;
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

    async fn cancel_pending_task(&self) {
        let pending = {
            let mut guard = self.inner.state.lock().await;
            guard.pending.take()
        };
        if let Some(pending) = pending {
            pending.abort_handle.abort();
        }
    }

    async fn run_browser_login(
        &self,
        listener: TcpListener,
        redirect_uri: String,
        pkce: PkceCodes,
        expected_state: String,
    ) -> io::Result<()> {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let request = read_http_request(&mut stream).await?;
            let request_line = request.lines().next().unwrap_or_default().to_string();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .to_string();

            if path == "/cancel" {
                write_http_response(&mut stream, "Login cancelled", false).await?;
                return Err(io::Error::other("Login cancelled"));
            }

            if !path.starts_with("/auth/callback") {
                write_http_response(
                    &mut stream,
                    "Waiting for OpenAI sign-in callback on /auth/callback",
                    false,
                )
                .await?;
                continue;
            }

            let url =
                url::Url::parse(&format!("http://localhost{path}")).map_err(io::Error::other)?;
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

            if let Some(error) = error {
                write_http_response(
                    &mut stream,
                    "OpenAI sign-in failed. You can return to Kraai.",
                    true,
                )
                .await?;
                return Err(io::Error::other(error));
            }

            if state.as_deref() != Some(expected_state.as_str()) {
                write_http_response(&mut stream, "OpenAI sign-in failed. State mismatch.", true)
                    .await?;
                return Err(io::Error::other("OpenAI sign-in state mismatch"));
            }

            let code = code.ok_or_else(|| io::Error::other("Missing OAuth code"))?;
            let auth = self
                .exchange_authorization_code(&redirect_uri, &pkce, &code)
                .await?;
            write_http_response(
                &mut stream,
                "OpenAI sign-in complete. You can return to Kraai.",
                true,
            )
            .await?;
            self.finish_successful_login(auth).await?;
            return Ok(());
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
    ) -> io::Result<()> {
        let started_at = unix_now();
        loop {
            if unix_now().saturating_sub(started_at) > DEVICE_CODE_TIMEOUT_SECS {
                return Err(io::Error::other("OpenAI device-code login timed out"));
            }

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
                self.finish_successful_login(auth).await?;
                return Ok(());
            }

            if status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND {
                tokio::time::sleep(Duration::from_secs(interval_seconds.max(1))).await;
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
    Ok(Some(StoredAuth {
        tokens: stored.tokens,
        claims,
        last_refresh_unix: stored.last_refresh,
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

async fn bind_listener(port: u16) -> io::Result<TcpListener> {
    let primary = SocketAddr::from(([127, 0, 0, 1], port));
    match TcpListener::bind(primary).await {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await
        }
        Err(error) => Err(error),
    }
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
    let mut buffer = [0u8; 4096];
    let size = stream.read(&mut buffer).await?;
    String::from_utf8(buffer[..size].to_vec()).map_err(io::Error::other)
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    message: &str,
    close: bool,
) -> io::Result<()> {
    let body =
        format!("<html><body><pre style=\"font-family: monospace\">{message}</pre></body></html>");
    let connection = if close { "close" } else { "keep-alive" };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: {connection}\r\n\r\n{}",
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
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
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

    fn test_client_or_skip() -> Option<Client> {
        match Client::builder().build() {
            Ok(client) => Some(client),
            Err(error) if is_missing_system_ca_error(&error) => None,
            Err(error) => panic!("unexpected reqwest client build error: {error}"),
        }
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

    #[test]
    fn id_token_claims_extract_email_plan_and_account_id() {
        let claims =
            parse_id_token_claims(&fake_jwt("user@example.com", "pro", "workspace_123")).unwrap();

        assert_eq!(claims.email.as_deref(), Some("user@example.com"));
        assert_eq!(claims.plan_type.as_deref(), Some("Pro"));
        assert_eq!(claims.account_id.as_deref(), Some("workspace_123"));
    }

    #[test]
    fn request_auth_applies_bearer_and_account_headers() {
        let auth = RequestAuth {
            access_token: "access-token".to_string(),
            account_id: "workspace_123".to_string(),
        };
        let Some(client) = test_client_or_skip() else {
            return;
        };
        let request = client
            .get("https://example.com")
            .bearer_auth(&auth.access_token)
            .header("ChatGPT-Account-Id", &auth.account_id)
            .build()
            .unwrap();

        assert_eq!(
            request.headers().get("authorization").unwrap(),
            "Bearer access-token"
        );
        assert_eq!(
            request.headers().get("ChatGPT-Account-Id").unwrap(),
            "workspace_123"
        );
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
        }
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
