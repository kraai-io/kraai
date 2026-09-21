use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRetryEvent {
    pub operation: &'static str,
    pub retry_number: u32,
    pub delay: Duration,
    pub reason: String,
}

pub trait ProviderRetryObserver: Send + Sync {
    fn on_retry_scheduled(&self, event: &ProviderRetryEvent);

    fn before_attempt(
        &self,
        _unpriced_prior_attempts: u32,
    ) -> Pin<Box<dyn Future<Output = color_eyre::Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Default)]
pub struct ProviderRequestContext {
    retry_observer: Option<Arc<dyn ProviderRetryObserver>>,
    prompt_cache_key: Option<String>,
}

impl ProviderRequestContext {
    pub fn new(retry_observer: Option<Arc<dyn ProviderRetryObserver>>) -> Self {
        Self {
            retry_observer,
            prompt_cache_key: None,
        }
    }

    pub fn with_retry_observer(retry_observer: Arc<dyn ProviderRetryObserver>) -> Self {
        Self {
            retry_observer: Some(retry_observer),
            prompt_cache_key: None,
        }
    }

    pub fn with_prompt_cache_key(prompt_cache_key: String) -> Self {
        Self {
            retry_observer: None,
            prompt_cache_key: Some(prompt_cache_key),
        }
    }

    pub fn with_retry_observer_and_prompt_cache_key(
        retry_observer: Arc<dyn ProviderRetryObserver>,
        prompt_cache_key: String,
    ) -> Self {
        Self {
            retry_observer: Some(retry_observer),
            prompt_cache_key: Some(prompt_cache_key),
        }
    }

    pub fn retry_observer(&self) -> Option<&dyn ProviderRetryObserver> {
        self.retry_observer.as_deref()
    }

    pub fn prompt_cache_key(&self) -> Option<&str> {
        self.prompt_cache_key.as_deref()
    }
}
