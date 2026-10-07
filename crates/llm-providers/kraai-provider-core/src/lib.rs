#![forbid(unsafe_code)]

mod cache_warming;
pub use cache_warming::{CacheWarmingPolicy, CacheWarmup};

mod config;
mod definition;
mod error;
mod history;
mod http_body;
#[cfg(test)]
mod http_tests;
pub use http_body::read_error_body;
mod http_retry;
pub use history::prepare_history;
mod images;
mod manager;
pub use images::{ImageResolver, ResolvedImages, validate_image_support};
mod model_metadata;
mod pricing;
pub use pricing::{PriceQuote, Pricing, ProviderPricingCatalog, ProviderPricingPolicy};
mod provider;
mod registry;
mod request_context;
mod sse;
mod stream;

#[cfg(test)]
mod test_support;

pub use config::{DynamicConfig, DynamicValue, ModelConfig, ProviderConfig, ProviderManagerConfig};
pub use definition::{FieldDefinition, FieldValueKind, ProviderDefinition, ValidationError};
pub use error::{ProviderError, ProviderModelCacheRefreshError};

pub use http_retry::{DEFAULT_HTTP_RETRY_POLICY, HttpRetryPolicy, send_with_retry};
pub use manager::ProviderManager;
pub use model_metadata::ConfiguredModelMetadata;
pub use provider::{Model, Provider, ProviderRequest, ScriptToolDefinition};
pub use registry::{ProviderFactory, ProviderRegistry};
pub use request_context::{ProviderRequestContext, ProviderRetryEvent, ProviderRetryObserver};
pub use sse::{MAX_SSE_EVENT_BYTES, SseEvent, stream_sse_data};
pub use stream::{ProviderStreamEvent, StreamStatus, adapt_provider_stream};

mod model_catalog;
pub use model_catalog::{CatalogModelMetadata, ModelCatalog};
