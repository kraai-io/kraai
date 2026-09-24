use std::pin::Pin;
use std::sync::Arc;

use color_eyre::Result;
use kraai_persistence::RequestUsageStore;
use kraai_provider_core::{ProviderManager, ProviderRetryEvent, ProviderRetryObserver};
use kraai_types::{MessageId, ModelId, ProviderId, RequestUsage, TokenUsage};
use tokio::sync::{Mutex, RwLock};

pub struct AuxiliaryUsageRecorder {
    pub store: Arc<dyn RequestUsageStore>,
    pub session_id: String,
    pub on_usage: Option<Arc<dyn Fn(RequestUsage) + Send + Sync>>,
    pub barrier: Option<Arc<RwLock<()>>>,
}

pub struct AuxiliaryRequestUsage {
    recorder: AuxiliaryUsageRecorder,
    request: Mutex<RequestUsage>,
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
            request: Mutex::new(request.clone()),
        });
        observer.persist(&request).await?;
        Ok(observer)
    }
}

impl AuxiliaryRequestUsage {
    pub async fn snapshot(&self) -> RequestUsage {
        self.request.lock().await.clone()
    }

    pub async fn save_usage(&self, usage: TokenUsage) -> Result<()> {
        let mut request = self.request.lock().await;
        request.usage = Some(usage);
        self.persist(&request).await?;
        drop(request);
        Ok(())
    }

    async fn persist(&self, request: &RequestUsage) -> Result<()> {
        let _guard = match &self.recorder.barrier {
            Some(barrier) => Some(barrier.read().await),
            None => None,
        };
        self.recorder
            .store
            .save(&self.recorder.session_id, request)
            .await?;
        if let Some(observer) = &self.recorder.on_usage {
            observer(request.clone());
        }
        Ok(())
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
                let mut request = self.request.lock().await;
                request.unpriced_attempts = attempts;
                self.persist(&request).await?;
                drop(request);
            }
            Ok(())
        })
    }
}
