use std::time::Duration;

use kraai_types::ProviderId;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use thiserror::Error;

use crate::rate_limit::RateLimit;

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
    #[error("Usage quota is exhausted and the provider gave no reset time: {0}")]
    QuotaExhausted(String),
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
        .map(|delay| format!(" (resets in {})", crate::rate_limit::describe(delay)))
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

    pub async fn from_response(
        provider: &str,
        operation: &str,
        response: reqwest::Response,
    ) -> Self {
        let status = response.status();
        let headers = response.headers().clone();
        let url = response.url().to_string();
        let body = crate::read_error_body(response).await;
        Self::from_body(
            status,
            &headers,
            &body,
            format!("{provider} {operation} failed with status {status} at {url}: {body}"),
        )
    }

    pub fn from_body(status: StatusCode, headers: &HeaderMap, body: &str, message: String) -> Self {
        if let Some(error) = Self::from_api_error(body) {
            return error;
        }
        match crate::rate_limit::classify(headers, body) {
            RateLimit::Exhausted => Self::QuotaExhausted(message),
            RateLimit::ResetsIn(delay) if status == StatusCode::TOO_MANY_REQUESTS => {
                Self::RateLimited {
                    message,
                    retry_after: Some(delay),
                }
            }
            RateLimit::ResetsIn(_) | RateLimit::Unknown => {
                Self::from_status(status, crate::rate_limit::retry_after(headers), message)
            }
        }
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

    pub fn from_stream_error(code: Option<&str>, message: String) -> Self {
        match code {
            Some("context_length_exceeded" | "context_window_exceeded") => {
                Self::ContextWindowExceeded(message)
            }
            Some(code) if crate::rate_limit::EXHAUSTED_ERRORS.contains(&code) => {
                Self::QuotaExhausted(message)
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

pub fn is_context_overflow_error(report: &color_eyre::Report) -> bool {
    report.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<ProviderError>(),
            Some(ProviderError::ContextWindowExceeded(_))
        )
    })
}

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
