use std::time::Duration;

use kraai_types::ProviderId;
use reqwest::StatusCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("Model context window exceeded: {0}")]
    ContextWindowExceeded(String),
    #[error("Provider stream interrupted: {0}")]
    StreamInterrupted(String),

    #[error("{message}{}", retry_after_hint(*.retry_after))]
    RateLimited {
        message: String,
        retry_after: Option<Duration>,
    },
    #[error("{message}")]
    Unavailable {
        status: Option<u16>,
        message: String,
    },
    #[error("{message}")]
    Authentication { status: u16, message: String },
    #[error("{message}")]
    Request { status: u16, message: String },
    #[error("{0}")]
    Transport(String),

    #[error("Provider not found: {0}")]
    ProviderNotFound(ProviderId),

    #[error("Unknown provider type: {0}")]
    UnknownProviderType(String),

    #[error("Failed to parse config: {0}")]
    ConfigParseError(String),

    #[error("Provider '{0}' not registered when trying to add model")]
    ProviderNotRegistered(ProviderId),

    #[error("Factory already registered for type: {0}")]
    FactoryAlreadyRegistered(String),

    #[error("Invalid config:\n{0}")]
    ConfigValidationError(String),

    #[error("Failed to refresh model cache for provider(s): {0:?}")]
    ModelCacheRefreshFailed(Vec<ProviderModelCacheRefreshError>),
}

fn retry_after_hint(retry_after: Option<Duration>) -> String {
    retry_after
        .map(|delay| format!(" (retry after {} s)", delay.as_secs()))
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelCacheRefreshError {
    pub provider_id: ProviderId,
    pub message: String,
}

impl ProviderError {
    pub fn from_api_error(body: &str) -> Option<Self> {
        matches!(
            api_error_code(body).as_deref(),
            Some("context_length_exceeded" | "context_window_exceeded")
        )
        .then(|| Self::ContextWindowExceeded(body.to_string()))
    }

    /// Classifies a failed HTTP response from `provider`'s `operation`.
    pub async fn from_response(
        provider: &str,
        operation: &str,
        response: reqwest::Response,
    ) -> Self {
        let status = response.status();
        let retry_after = crate::http_retry::parse_retry_after(&response);
        let url = response.url().to_string();
        let body = crate::read_error_body(response).await;
        Self::from_body(
            status,
            retry_after,
            &body,
            format!("{provider} {operation} failed with status {status} at {url}: {body}"),
        )
    }

    /// Classifies a failed response whose body the caller already read.
    pub fn from_body(
        status: StatusCode,
        retry_after: Option<Duration>,
        body: &str,
        message: String,
    ) -> Self {
        Self::from_api_error(body)
            .unwrap_or_else(|| Self::from_status(status, retry_after, message))
    }

    pub fn from_status(status: StatusCode, retry_after: Option<Duration>, message: String) -> Self {
        match status {
            StatusCode::TOO_MANY_REQUESTS => Self::RateLimited {
                message,
                retry_after,
            },
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Self::Authentication {
                status: status.as_u16(),
                message,
            },
            StatusCode::REQUEST_TIMEOUT | StatusCode::CONFLICT => Self::Unavailable {
                status: Some(status.as_u16()),
                message,
            },
            status if status.is_server_error() => Self::Unavailable {
                status: Some(status.as_u16()),
                message,
            },
            status => Self::Request {
                status: status.as_u16(),
                message,
            },
        }
    }

    /// Classifies an error reported inside a response stream by its API error code.
    pub fn from_stream_error(code: Option<&str>, message: String) -> Self {
        match code {
            Some("context_length_exceeded" | "context_window_exceeded") => {
                Self::ContextWindowExceeded(message)
            }
            Some("rate_limit_exceeded") => Self::RateLimited {
                message,
                retry_after: None,
            },
            Some("server_error" | "overloaded" | "overloaded_error" | "server_is_overloaded") => {
                Self::Unavailable {
                    status: None,
                    message,
                }
            }
            _ => Self::Request { status: 0, message },
        }
    }

    /// Whether repeating the identical request may succeed.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::StreamInterrupted(_)
                | Self::RateLimited { .. }
                | Self::Unavailable { .. }
                | Self::Transport(_)
        )
    }
}

/// Whether the request was too large for the model's context window.
pub fn is_context_overflow_error(report: &color_eyre::Report) -> bool {
    report.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<ProviderError>(),
            Some(ProviderError::ContextWindowExceeded(_))
        )
    })
}

/// Whether a provider failure anywhere in `report`'s chain may succeed on an identical retry.
pub fn is_transient_error(report: &color_eyre::Report) -> bool {
    report.chain().any(|cause| {
        cause
            .downcast_ref::<ProviderError>()
            .is_some_and(ProviderError::is_transient)
            || cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(is_transient_transport)
    })
}

fn is_transient_transport(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_body() || error.is_request()
}

fn api_error_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .pointer("/error/code")
        .or_else(|| value.get("code"))?
        .as_str()
        .map(str::to_string)
}
