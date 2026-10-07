use std::time::Duration;

use reqwest::{Client, ClientBuilder, RequestBuilder, Response};

use crate::read::ReadPrefix;

pub const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const HTTP_FINITE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const HTTP_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Default)]
pub struct HttpTimeouts {
    pub connect: Option<Duration>,
    pub read: Option<Duration>,
    pub request: Option<Duration>,
}

impl HttpTimeouts {
    pub const STREAMING: Self = Self {
        connect: Some(HTTP_CONNECT_TIMEOUT),
        read: Some(HTTP_STREAM_IDLE_TIMEOUT),
        request: None,
    };
    pub const FINITE: Self = Self {
        request: Some(HTTP_FINITE_REQUEST_TIMEOUT),
        ..Self::STREAMING
    };
}

pub fn client_builder(
    timeouts: HttpTimeouts,
    redirects: reqwest::redirect::Policy,
) -> ClientBuilder {
    let mut builder = Client::builder().redirect(redirects);
    if let Some(timeout) = timeouts.connect {
        builder = builder.connect_timeout(timeout);
    }
    if let Some(timeout) = timeouts.read {
        builder = builder.read_timeout(timeout);
    }
    if let Some(timeout) = timeouts.request {
        builder = builder.timeout(timeout);
    }
    builder
}

pub fn build_streaming_http_client() -> reqwest::Result<Client> {
    streaming_http_client_builder().build()
}

pub fn streaming_http_client_builder() -> ClientBuilder {
    client_builder(
        HttpTimeouts::STREAMING,
        reqwest::redirect::Policy::default(),
    )
}

pub fn build_finite_http_client() -> reqwest::Result<Client> {
    client_builder(HttpTimeouts::FINITE, reqwest::redirect::Policy::default()).build()
}

pub fn finite_request(builder: RequestBuilder) -> RequestBuilder {
    builder.timeout(HTTP_FINITE_REQUEST_TIMEOUT)
}

#[derive(Debug, thiserror::Error)]
pub enum BodyReadError {
    #[error(transparent)]
    Transport(#[from] reqwest::Error),
    #[error("HTTP response body exceeds {limit} bytes")]
    TooLarge { limit: usize },
}

pub async fn read_response_body(
    response: Response,
    limit: usize,
) -> Result<Vec<u8>, BodyReadError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(BodyReadError::TooLarge { limit });
    }
    let prefix = read_response_prefix(response, limit).await?;
    if prefix.truncated {
        return Err(BodyReadError::TooLarge { limit });
    }
    Ok(prefix.bytes)
}

pub async fn read_response_prefix(
    mut response: Response,
    limit: usize,
) -> Result<ReadPrefix, reqwest::Error> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend(chunk.iter().take(remaining));
        if chunk.len() > remaining {
            return Ok(ReadPrefix {
                bytes,
                truncated: true,
            });
        }
    }
    Ok(ReadPrefix {
        bytes,
        truncated: false,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct TextPrefix {
    pub text: String,
    pub truncated: bool,
}

pub async fn read_response_text_prefix(
    response: Response,
    limit: usize,
) -> Result<TextPrefix, reqwest::Error> {
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .cloned();
    let prefix = read_response_prefix(response, limit).await?;
    let mut bounded = http::Response::new(prefix.bytes);
    if let Some(content_type) = content_type {
        bounded
            .headers_mut()
            .insert(reqwest::header::CONTENT_TYPE, content_type);
    }
    Ok(TextPrefix {
        text: Response::from(bounded).text().await?,
        truncated: prefix.truncated,
    })
}

#[cfg(test)]
mod tests;
