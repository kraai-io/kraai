#![expect(
    clippy::unwrap_used,
    reason = "usage commit tests assert cancellation and barrier ownership"
)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{Notify, Semaphore};

use super::*;

struct BlockedStore {
    entered: Notify,
    release: Semaphore,
    requests: Mutex<Vec<RequestUsage>>,
}

impl Default for BlockedStore {
    fn default() -> Self {
        Self {
            entered: Notify::new(),
            release: Semaphore::new(0),
            requests: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl RequestUsageStore for BlockedStore {
    async fn save(&self, _session_id: &str, request: &RequestUsage) -> Result<()> {
        self.entered.notify_one();
        self.release.acquire().await?.forget();
        self.requests.lock().await.push(request.clone());
        Ok(())
    }

    async fn delete(&self, _session_id: &str) -> Result<()> {
        self.requests.lock().await.clear();
        Ok(())
    }

    async fn load(&self, _session_id: &str) -> Result<BTreeMap<MessageId, RequestUsage>> {
        Ok(self
            .requests
            .lock()
            .await
            .iter()
            .map(|request| (request.message_id.clone(), request.clone()))
            .collect())
    }
}

#[tokio::test]
async fn cancelled_usage_commit_blocks_shutdown_until_save_and_observer_finish() {
    let store = Arc::new(BlockedStore::default());
    let barrier = Arc::new(RwLock::new(()));
    let observed = Arc::new(AtomicBool::new(false));
    let observer_had_barrier = Arc::new(AtomicBool::new(false));
    let request = RequestUsage {
        message_id: MessageId::new("auxiliary"),
        provider_id: ProviderId::new("provider"),
        model_id: ModelId::new("model"),
        started_at: 1,
        subscription: false,
        unpriced_attempts: 0,
        usage: None,
    };
    let usage = Arc::new(AuxiliaryRequestUsage {
        recorder: AuxiliaryUsageRecorder {
            store: store.clone(),
            session_id: String::from("session"),
            barrier: Some(barrier.clone()),
            on_usage: Some(Arc::new({
                let observed = observed.clone();
                let observer_had_barrier = observer_had_barrier.clone();
                let barrier = barrier.clone();
                move |_| {
                    observer_had_barrier.store(barrier.try_write().is_err(), Ordering::SeqCst);
                    observed.store(true, Ordering::SeqCst);
                }
            })),
        },
        request: Arc::new(Mutex::new(request)),
    });
    let caller = tokio::spawn({
        let usage = usage.clone();
        async move { usage.before_attempt(1).await }
    });
    store.entered.notified().await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(usage.request.try_lock().is_err());
    assert!(!observed.load(Ordering::SeqCst));
    let mut shutdown = std::pin::pin!(barrier.write());
    assert!(futures::poll!(&mut shutdown).is_pending());
    store.release.add_permits(1);
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
        .await
        .unwrap();
    assert!(observed.load(Ordering::SeqCst));
    assert!(observer_had_barrier.load(Ordering::SeqCst));
    let requests = store.requests.lock().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests.first().unwrap().unpriced_attempts, 1);
    drop(requests);
    assert_eq!(usage.snapshot().await.unpriced_attempts, 1);
    drop(completed);
}
