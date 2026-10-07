use std::pin::Pin;
use std::sync::Arc;

use color_eyre::eyre::{Context, Result};
use kraai_persistence::RequestUsageStore;
use kraai_provider_core::{ProviderManager, ProviderRetryEvent, ProviderRetryObserver};
use kraai_types::{MessageId, ModelId, ProviderId, RequestUsage, TokenUsage};
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};

pub struct AuxiliaryUsageRecorder {
    pub store: Arc<dyn RequestUsageStore>,
    pub session_id: String,
    pub on_usage: Option<Arc<dyn Fn(RequestUsage) + Send + Sync>>,
    pub barrier: Option<Arc<RwLock<()>>>,
}

pub struct AuxiliaryRequestUsage {
    recorder: AuxiliaryUsageRecorder,
    request: Arc<Mutex<RequestUsage>>,
}

impl AuxiliaryUsageRecorder {
    pub async fn start(
        self,
        providers: &ProviderManager,
        provider_id: &ProviderId,
        model_id: &ModelId,
        purpose: &str,
    ) -> Result<Arc<AuxiliaryRequestUsage>> {
        let request = RequestUsage {
            message_id: MessageId::new(format!("{purpose}-{}", ulid::Ulid::generate())),
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            started_at: u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
            )
            .unwrap_or(u64::MAX),
            subscription: providers.is_subscription(provider_id),
            unpriced_attempts: 0,
            usage: None,
        };
        let observer = Arc::new(AuxiliaryRequestUsage {
            recorder: self,
            request: Arc::new(Mutex::new(request)),
        });
        {
            let request = observer.request.clone().lock_owned().await;
            observer.persist(request).await
        }?;
        Ok(observer)
    }
}

impl AuxiliaryRequestUsage {
    pub async fn snapshot(&self) -> RequestUsage {
        self.request.lock().await.clone()
    }

    pub async fn save_usage(&self, usage: TokenUsage) -> Result<()> {
        let mut request = self.request.clone().lock_owned().await;
        request.usage = Some(usage);
        self.persist(request).await
    }

    async fn persist(&self, request: OwnedMutexGuard<RequestUsage>) -> Result<()> {
        let guard = match &self.recorder.barrier {
            Some(barrier) => Some(barrier.clone().read_owned().await),
            None => None,
        };
        let store = self.recorder.store.clone();
        let session_id = self.recorder.session_id.clone();
        let observer = self.recorder.on_usage.clone();
        tokio::spawn(async move {
            store.save(&session_id, &request).await?;
            if let Some(observer) = observer {
                observer(request.clone());
            }
            drop(request);
            drop(guard);
            Ok(())
        })
        .await
        .context("Auxiliary request usage commit task failed")?
    }
}

impl ProviderRetryObserver for AuxiliaryRequestUsage {
    fn on_retry_scheduled(&self, _event: &ProviderRetryEvent) {}

    fn before_attempt(
        &self,
        attempts: u32,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if attempts > 0 {
                let mut request = self.request.clone().lock_owned().await;
                request.unpriced_attempts = attempts;
                self.persist(request).await
            } else {
                Ok(())
            }
        })
    }
}

#[cfg(test)]
mod tests;
