use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use rand::Rng;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::token::{StoredAuth, StoredTokens, generate_generation, parse_id_token_claims};
use super::{AuthConfig, DEFAULT_ORIGINATOR, unix_now};

const DEVICE_CODE_TIMEOUT_SECS: u64 = 15 * 60;
const CALLBACK_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CALLBACK_HEADER_BYTES: usize = 16 * 1024;

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

#[derive(Clone)]
pub(super) struct PkceCodes {
    code_verifier: String,
    code_challenge: String,
}

pub(super) struct DeviceCodeResponseData {
    pub(super) device_auth_id: String,
    pub(super) user_code: String,
    pub(super) interval_seconds: u64,
}

pub(super) async fn run_browser_login(
    client: &Client,
    config: &AuthConfig,
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
                write_http_response(&mut stream, "400 Bad Request", "Invalid callback URL").await?;
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
        let auth = exchange_authorization_code(client, config, &redirect_uri, &pkce, &code).await?;
        write_http_response(
            &mut stream,
            "200 OK",
            "OpenAI sign-in complete. You can return to Kraai.",
        )
        .await?;
        return Ok(auth);
    }
}

pub(super) async fn request_device_code(
    client: &Client,
    config: &AuthConfig,
) -> io::Result<DeviceCodeResponseData> {
    let response = client
        .post(format!(
            "{}/api/accounts/deviceauth/usercode",
            config.issuer
        ))
        .json(&DeviceCodeRequest {
            client_id: &config.client_id,
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

pub(super) async fn run_device_code_login(
    client: &Client,
    config: &AuthConfig,
    device_auth_id: String,
    user_code: String,
    interval_seconds: u64,
    verification_url: String,
) -> io::Result<StoredAuth> {
    tokio::time::timeout(
        Duration::from_secs(DEVICE_CODE_TIMEOUT_SECS),
        run_device_code_login_inner(
            client,
            config,
            device_auth_id,
            user_code,
            interval_seconds,
            verification_url,
        ),
    )
    .await
    .map_err(|_elapsed| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "OpenAI device-code login timed out",
        )
    })?
}

async fn run_device_code_login_inner(
    client: &Client,
    config: &AuthConfig,
    device_auth_id: String,
    user_code: String,
    interval_seconds: u64,
    verification_url: String,
) -> io::Result<StoredAuth> {
    let interval = Duration::from_secs(interval_seconds.clamp(1, 30));
    loop {
        let response = client
            .post(format!("{}/api/accounts/deviceauth/token", config.issuer))
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
            let auth = exchange_authorization_code(
                client,
                config,
                &format!("{}/deviceauth/callback", config.issuer),
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
    client: &Client,
    config: &AuthConfig,
    redirect_uri: &str,
    pkce: &PkceCodes,
    code: &str,
) -> io::Result<StoredAuth> {
    let response = client
        .post(format!("{}/oauth/token", config.issuer))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
            urlencoding::encode(code),
            urlencoding::encode(redirect_uri),
            urlencoding::encode(&config.client_id),
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

pub(super) async fn bind_listener(port: u16, fallback_ports: &[u16]) -> io::Result<TcpListener> {
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

pub(super) fn build_authorize_url(
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

pub(super) fn generate_pkce() -> PkceCodes {
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

pub(super) fn generate_state() -> String {
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
            let read_buffer = buffer.get_mut(..read_capacity).ok_or_else(|| {
                io::Error::other("callback read capacity exceeded the receive buffer")
            })?;
            let size = stream.read(read_buffer).await?;
            if size == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "callback connection closed before HTTP headers completed",
                ));
            }
            let read_bytes = buffer
                .get(..size)
                .ok_or_else(|| io::Error::other("callback read exceeded the receive buffer"))?;
            request.extend_from_slice(read_bytes);
        }
    })
    .await
    .map_err(|_elapsed| io::Error::new(io::ErrorKind::TimedOut, "callback request timed out"))?
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

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "tests use direct assertions for callback fixtures"
)]
mod tests {
    use super::*;

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
}
