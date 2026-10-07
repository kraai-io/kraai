use std::time::Duration;

use rmcp::transport::auth::{
    AuthorizationManager, AuthorizationRequest, AuthorizationSession, CredentialStore,
    OAuthClientConfig,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::store::FileStore;
use crate::OAuthConfig;

pub(super) fn validate_url(value: &str, allow_loopback: bool) -> Result<url::Url, String> {
    let url = url::Url::parse(value).map_err(|error| error.to_string())?;
    let local = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
            || host == "[::1]"
    });
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || allow_loopback && local && url.scheme() == "http")
    {
        return Err(String::from(
            "OAuth requires HTTPS, except for a local HTTP server on loopback",
        ));
    }
    Ok(url)
}

pub(super) async fn manager(url: &str, store: FileStore) -> Result<AuthorizationManager, String> {
    validate_url(url, true)?;
    let client = kraai_io::http::client_builder(
        kraai_io::http::HttpTimeouts {
            request: Some(Duration::from_secs(30)),
            ..Default::default()
        },
        reqwest::redirect::Policy::none(),
    )
    .build()
    .map_err(|error| error.to_string())?;
    let response = client
        .get(url)
        .header("accept", "text/event-stream")
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let challenge = response
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    drop(response);
    let endpoint = std::sync::Arc::new(std::sync::OnceLock::new());
    let oauth_client = super::registration::RegistrationClient {
        inner: rmcp::transport::auth::default_oauth_http_client()
            .map_err(|error| error.to_string())?,
        endpoint: endpoint.clone(),
        store: store.clone(),
    };
    let mut manager =
        AuthorizationManager::new_with_oauth_http_client(url, std::sync::Arc::new(oauth_client))
            .await
            .map_err(|error| error.to_string())?;
    manager.set_credential_store(store);
    let resolution = manager
        .resolve_metadata_from_challenge(challenge.as_deref())
        .await
        .map_err(|error| error.to_string())?;
    if !resolution.source.is_discovered() {
        return Err(String::from("MCP server did not advertise OAuth metadata"));
    }
    let allow_loopback = url::Url::parse(url).is_ok_and(|url| url.scheme() == "http");
    for endpoint in [
        Some(&resolution.metadata.authorization_endpoint),
        Some(&resolution.metadata.token_endpoint),
        resolution.metadata.registration_endpoint.as_ref(),
        resolution.metadata.issuer.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        validate_url(endpoint, allow_loopback)?;
    }
    if let Some(url) = &resolution.metadata.registration_endpoint {
        let _ = endpoint.set(validate_url(url, allow_loopback)?);
    }
    manager.set_metadata(resolution.metadata);
    Ok(manager)
}

fn client_secret(config: &OAuthConfig) -> Result<Option<String>, String> {
    config
        .client_secret_env
        .as_ref()
        .map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    format!("Missing MCP OAuth client secret environment variable {name}")
                })
        })
        .transpose()
}

pub(super) async fn restore_client(
    manager: &mut AuthorizationManager,
    config: &OAuthConfig,
    store: &FileStore,
) -> Result<(), String> {
    if let Some(credentials) = store.load().await.map_err(|error| error.to_string())? {
        let client_id = &credentials.client_id;
        let mut client = OAuthClientConfig::new(client_id, "http://127.0.0.1/auth/callback");
        client.client_secret = client_secret(config)?.or(store
            .registration_secret()
            .map_err(|error| error.to_string())?);
        manager
            .configure_client(client)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(super) async fn start(
    url: &str,
    config: &OAuthConfig,
    store: FileStore,
) -> Result<(AuthorizationSession, TcpListener), String> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.redirect_port))
        .await
        .map_err(|error| error.to_string())?;
    let redirect_uri = format!(
        "http://127.0.0.1:{}/auth/callback",
        listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port()
    );
    let manager = manager(url, store).await?;
    let scope_hint = (!config.scopes.is_empty()).then(|| config.scopes.join(" "));
    let scopes = manager.select_scopes(scope_hint.as_deref(), &[]);
    let mut request = AuthorizationRequest::new(redirect_uri)
        .with_client_name("Kraai")
        .with_application_type("native")
        .with_scopes(scopes);
    if let Some(client_id) = &config.client_id {
        request = request.with_preregistered_client(client_id);
    }
    if let Some(secret) = client_secret(config)? {
        request = request.with_client_secret(secret);
    }
    if let Some(metadata_url) = &config.client_metadata_url {
        request = request.with_client_metadata_url(metadata_url);
    }
    let session = AuthorizationSession::new(manager, request)
        .await
        .map_err(|(_, error)| error.to_string())?;
    Ok((session, listener))
}

async fn read_callback(stream: &mut TcpStream) -> Result<url::Url, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err(String::from("Incomplete callback"));
        }
        bytes.extend_from_slice(buffer.get(..read).ok_or("Invalid callback")?);
        if bytes.len() > 16384 {
            return Err(String::from("Callback is too large"));
        }
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
    let mut line = request
        .lines()
        .next()
        .ok_or("Missing request line")?
        .split_whitespace();
    if line.next() != Some("GET") {
        return Err(String::from("Expected GET callback"));
    }
    let target = line.next().ok_or("Missing callback target")?;
    if !target.starts_with("/auth/callback?") {
        return Err(String::from("Unknown callback path"));
    }
    url::Url::parse(&format!("http://127.0.0.1{target}")).map_err(|error| error.to_string())
}

async fn respond(stream: &mut TcpStream, success: bool) {
    let (status, body) = if success {
        ("200 OK", "Signed in to MCP. You can close this window.")
    } else {
        (
            "400 Bad Request",
            "Invalid or unsuccessful OAuth callback. Return to Kraai for login status.",
        )
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        stream.write_all(response.as_bytes()),
    )
    .await;
}

pub(super) async fn complete(
    session: AuthorizationSession,
    listener: TcpListener,
    store: FileStore,
) -> Result<(), String> {
    let auth_url = url::Url::parse(&session.auth_url).map_err(|error| error.to_string())?;
    let expected_state = auth_url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .ok_or("Missing OAuth state")?;
    loop {
        let (mut stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
        let Ok(Ok(callback)) =
            tokio::time::timeout(Duration::from_secs(2), read_callback(&mut stream)).await
        else {
            respond(&mut stream, false).await;
            continue;
        };
        let mut params = std::collections::BTreeMap::new();
        let mut duplicate = false;
        for (key, value) in callback.query_pairs() {
            duplicate |= params
                .insert(key.into_owned(), value.into_owned())
                .is_some();
        }
        if duplicate || params.get("state") != Some(&expected_state) {
            respond(&mut stream, false).await;
            continue;
        }
        if params.contains_key("error") {
            respond(&mut stream, false).await;
            return Err(String::from(
                "MCP login was denied by the authorization server",
            ));
        }
        let Some(code) = params.get("code") else {
            respond(&mut stream, false).await;
            continue;
        };
        let _guard = store.lock().await.map_err(|error| error.to_string())?;
        let result = session
            .handle_callback_with_issuer(
                code,
                &expected_state,
                params.get("iss").map(String::as_str),
            )
            .await;
        respond(&mut stream, result.is_ok()).await;
        match result {
            Ok(_) => return Ok(()),
            Err(
                rmcp::transport::auth::AuthError::AuthorizationServerMismatch { .. }
                | rmcp::transport::auth::AuthError::AuthorizationServerMissingIssuer { .. },
            ) => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
}
