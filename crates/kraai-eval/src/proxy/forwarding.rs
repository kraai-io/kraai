use std::io;
use std::sync::atomic::Ordering;

use color_eyre::eyre::{Context, Result, bail};
use futures::{Stream, StreamExt};
use reqwest::Method;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use super::headers::{forward_request_headers, response_head};
use super::{
    CacheState, DownstreamDelivery, ForwardOutcome, MAX_RESPONSE_BODY_BYTES, ParsedRequest,
    ProxyState, UpstreamCredentials, constant_time_eq,
};

pub(super) async fn forward_request<W>(
    stream: &mut W,
    state: &ProxyState,
    request: &ParsedRequest,
    outcome: &mut ForwardOutcome,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if !state.allowed_paths.contains(&request.path) {
        *outcome = ForwardOutcome::rejected(404);
        write_error(stream, 404, "Not Found").await?;
        return Ok(());
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
        *outcome = ForwardOutcome::rejected(401);
        write_error(stream, 401, "Unauthorized").await?;
        return Ok(());
    }
    let method = Method::from_bytes(request.method.as_bytes())?;
    if !matches!(method, Method::GET | Method::POST) {
        *outcome = ForwardOutcome::rejected(405);
        write_error(stream, 405, "Method Not Allowed").await?;
        return Ok(());
    }
    if state.request_count.fetch_add(1, Ordering::Relaxed) >= state.max_requests {
        *outcome = ForwardOutcome::rejected(429);
        write_error(stream, 429, "Proxy Request Limit Exceeded").await?;
        return Ok(());
    }
    outcome.stage = "upstream_headers";
    let response = match send_upstream(state, method, request).await {
        Ok(response) => response,
        Err(error) => {
            *outcome = ForwardOutcome::rejected(502);
            write_error(stream, 502, "Bad Gateway")
                .await
                .wrap_err_with(|| format!("upstream request failed: {error:#}"))?;
            outcome.stage = "upstream_headers";
            return Err(error.wrap_err("upstream request failed"));
        }
    };
    let status = response.status();
    outcome.status = Some(status.as_u16());
    outcome.response_cache_state = CacheState::from_response(response.headers());
    outcome.upstream_request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    outcome.delivery = DownstreamDelivery::Complete;
    outcome.stage = "response_headers";
    write_downstream(
        stream,
        &mut outcome.delivery,
        &response_head(status, response.headers()),
    )
    .await
    .wrap_err("forwarding response headers")?;
    relay_response(stream, outcome, response.bytes_stream()).await
}

pub(super) async fn relay_response<W, S, B, E>(
    stream: &mut W,
    outcome: &mut ForwardOutcome,
    body: S,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    S: Stream<Item = std::result::Result<B, E>>,
    B: AsRef<[u8]>,
    E: Into<color_eyre::Report>,
{
    futures::pin_mut!(body);
    loop {
        outcome.stage = "upstream_body";
        let Some(chunk) = body.next().await else {
            break;
        };
        let chunk = chunk
            .map_err(Into::into)
            .wrap_err("reading upstream response body")?;
        let chunk = chunk.as_ref();
        if outcome.body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BODY_BYTES {
            bail!("model proxy response body exceeds limit");
        }
        outcome.body.extend_from_slice(chunk);
        if chunk.is_empty() {
            continue;
        }
        outcome.stage = "downstream_body";
        write_downstream(
            stream,
            &mut outcome.delivery,
            format!("{:x}\r\n", chunk.len()).as_bytes(),
        )
        .await?;
        write_downstream(stream, &mut outcome.delivery, chunk).await?;
        write_downstream(stream, &mut outcome.delivery, b"\r\n").await?;
    }
    outcome.stage = "downstream_body";
    write_downstream(stream, &mut outcome.delivery, b"0\r\n\r\n").await?;
    outcome.stage = "complete";
    Ok(())
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

pub(super) async fn write_error<W>(stream: &mut W, status: u16, reason: &str) -> Result<()>
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
