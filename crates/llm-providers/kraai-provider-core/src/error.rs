use kraai_types::ProviderId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("Model context window exceeded: {0}")]
    ContextWindowExceeded(String),
    #[error("Provider stream interrupted: {0}")]
    StreamInterrupted(String),

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelCacheRefreshError {
    pub provider_id: ProviderId,
    pub message: String,
}

impl ProviderError {
    pub fn from_api_error(body: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        let code = value
            .pointer("/error/code")
            .or_else(|| value.get("code"))?
            .as_str()?;
        matches!(code, "context_length_exceeded" | "context_window_exceeded")
            .then(|| Self::ContextWindowExceeded(body.to_string()))
    }
}
