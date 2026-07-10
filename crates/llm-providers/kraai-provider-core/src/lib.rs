#![forbid(unsafe_code)]

mod config;
mod definition;
mod error;
mod http_retry;
mod manager;
mod provider;
mod registry;
mod sse;
mod stream;

#[cfg(test)]
mod test_support;

pub use config::{DynamicConfig, DynamicValue, ModelConfig, ProviderConfig, ProviderManagerConfig};
pub use definition::{FieldDefinition, FieldValueKind, ProviderDefinition, ValidationError};
pub use error::{ProviderError, ProviderModelCacheRefreshError};
pub use http_retry::{
    DEFAULT_HTTP_RETRY_POLICY, HttpRetryPolicy, ProviderRequestContext, ProviderRetryEvent,
    ProviderRetryObserver, send_with_retry,
};
pub use manager::ProviderManager;
pub use provider::{Model, Provider};
pub use registry::{ProviderFactory, ProviderRegistry};
pub use sse::{SseEvent, stream_sse_data};
pub use stream::ProviderStreamEvent;
