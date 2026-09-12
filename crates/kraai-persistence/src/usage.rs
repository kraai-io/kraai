use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use color_eyre::eyre::{Result, eyre};
use kraai_types::{MessageId, RequestUsage};
use tokio::fs;

#[derive(Default)]
struct SessionRequests {
    requests: tokio::sync::RwLock<Option<BTreeMap<MessageId, RequestUsage>>>,
    io: tokio::sync::Mutex<()>,
    revision: AtomicU64,
}

pub struct RequestUsageStore {
    root: PathBuf,
    hot: tokio::sync::RwLock<BTreeMap<String, Arc<SessionRequests>>>,
}

impl RequestUsageStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("usage"),
            hot: Default::default(),
        }
    }

    fn session_dir(&self, session_id: &str) -> Result<PathBuf> {
        MessageId::try_new(session_id).map_err(|error| eyre!(error))?;
        Ok(self.root.join(session_id))
    }

    async fn session_cache(&self, session_id: &str) -> Result<Arc<SessionRequests>> {
        self.session_dir(session_id)?;
        if let Some(cache) = self.hot.read().await.get(session_id) {
            return Ok(cache.clone());
        }
        let mut hot = self.hot.write().await;
        let cache = hot.entry(session_id.to_string()).or_default().clone();
        drop(hot);
        Ok(cache)
    }

    pub async fn save(&self, session_id: &str, request: &RequestUsage) -> Result<()> {
        MessageId::try_new(request.message_id.as_str()).map_err(|error| eyre!(error))?;
        let path = self
            .session_dir(session_id)?
            .join(format!("{}.json", request.message_id));
        let cache = self.session_cache(session_id).await?;
        let io = cache.io.lock().await;
        crate::atomic_write(&path, &serde_json::to_vec(request)?).await?;
        if let Some(requests) = cache.requests.write().await.as_mut() {
            requests.insert(request.message_id.clone(), request.clone());
        }
        cache.revision.fetch_add(1, Ordering::Release);
        drop(io);
        Ok(())
    }

    pub async fn delete(&self, session_id: &str) -> Result<()> {
        let path = self.session_dir(session_id)?;
        let cache = self.session_cache(session_id).await?;
        let io = cache.io.lock().await;
        match fs::remove_dir_all(path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        *cache.requests.write().await = Some(BTreeMap::new());
        cache.revision.fetch_add(1, Ordering::Release);
        drop(io);
        Ok(())
    }

    pub async fn load(&self, session_id: &str) -> Result<BTreeMap<MessageId, RequestUsage>> {
        let cache = self.session_cache(session_id).await?;
        if let Some(requests) = cache.requests.read().await.as_ref() {
            return Ok(requests.clone());
        }
        let io = cache.io.lock().await;
        if let Some(requests) = cache.requests.read().await.as_ref() {
            return Ok(requests.clone());
        }
        let requests = Self::read_requests(&self.session_dir(session_id)?).await?;
        *cache.requests.write().await = Some(requests.clone());
        drop(io);
        Ok(requests)
    }

    pub async fn refresh(&self, session_id: &str) -> Result<()> {
        let cache = self.session_cache(session_id).await?;
        loop {
            let revision = cache.revision.load(Ordering::Acquire);
            let requests = Self::read_requests(&self.session_dir(session_id)?).await;
            if Self::publish_refresh(&cache, revision, requests).await? {
                return Ok(());
            }
        }
    }

    async fn publish_refresh(
        cache: &SessionRequests,
        revision: u64,
        requests: Result<BTreeMap<MessageId, RequestUsage>>,
    ) -> Result<bool> {
        let io = cache.io.lock().await;
        if revision != cache.revision.load(Ordering::Acquire) {
            return Ok(false);
        }
        let requests = match requests {
            Ok(requests) => requests,
            Err(error) => {
                let cached = cache.requests.read().await.is_some();
                drop(io);
                if cached {
                    tracing::warn!(%error, "Using cached request usage after refresh failed");
                    return Ok(true);
                }
                return Err(error);
            }
        };
        *cache.requests.write().await = Some(requests);
        cache.revision.fetch_add(1, Ordering::Release);
        drop(io);
        Ok(true)
    }

    async fn read_requests(directory: &Path) -> Result<BTreeMap<MessageId, RequestUsage>> {
        let mut requests = BTreeMap::new();
        let mut entries = match fs::read_dir(directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(requests);
            }
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let request: RequestUsage = serde_json::from_slice(&fs::read(entry.path()).await?)?;
                requests.insert(request.message_id.clone(), request);
            }
        }
        Ok(requests)
    }
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible filesystem fixtures use direct assertions"
)]
mod tests {
    use super::*;
    use kraai_types::{ModelId, ProviderId};

    fn request() -> RequestUsage {
        RequestUsage {
            message_id: MessageId::new("request"),
            provider_id: ProviderId::new("provider"),
            model_id: ModelId::new("model"),
            started_at: 1,
            subscription: false,
            unpriced_attempts: 0,
            usage: None,
        }
    }

    #[tokio::test]
    async fn failed_refresh_uses_initialized_cache_and_recovers() -> Result<()> {
        let directory =
            std::env::temp_dir().join(format!("kraai-usage-{}", ulid::Ulid::generate()));
        let store = RequestUsageStore::new(&directory);
        let mut request = request();
        store.save("session", &request).await?;
        let cached = store.load("session").await?;
        let receipt_path = store.session_dir("session")?.join("request.json");
        fs::write(&receipt_path, b"invalid json").await?;
        store.refresh("session").await?;
        assert_eq!(store.load("session").await?, cached);
        let cold = RequestUsageStore::new(&directory);
        assert!(cold.refresh("session").await.is_err());
        assert!(cold.load("session").await.is_err());
        request.unpriced_attempts = 1;
        cold.save("session", &request).await?;
        store.refresh("session").await?;
        assert_eq!(
            store.load("session").await?.get(&request.message_id),
            Some(&request)
        );
        fs::remove_dir_all(directory).await?;
        Ok(())
    }

    #[tokio::test]
    async fn refresh_observes_other_store_writes_and_local_updates() -> Result<()> {
        let directory =
            std::env::temp_dir().join(format!("kraai-usage-{}", ulid::Ulid::generate()));
        let first = RequestUsageStore::new(&directory);
        let second = RequestUsageStore::new(&directory);
        let mut request = request();
        first.save("session", &request).await?;
        first.load("session").await?;
        request.unpriced_attempts = 1;
        second.save("session", &request).await?;
        first.refresh("session").await?;
        assert_eq!(
            first.load("session").await?.get(&request.message_id),
            Some(&request)
        );
        request.unpriced_attempts = 2;
        first.save("session", &request).await?;
        assert_eq!(
            first.load("session").await?.get(&request.message_id),
            Some(&request)
        );
        second.delete("session").await?;
        first.refresh("session").await?;
        assert!(first.load("session").await?.is_empty());
        fs::remove_dir_all(directory).await?;
        Ok(())
    }

    #[tokio::test]
    async fn refresh_rejects_reads_superseded_by_save_or_delete() -> Result<()> {
        let directory =
            std::env::temp_dir().join(format!("kraai-usage-{}", ulid::Ulid::generate()));
        let store = RequestUsageStore::new(&directory);
        let mut request = request();
        store.save("session", &request).await?;
        let cache = store.session_cache("session").await?;
        let revision = cache.revision.load(Ordering::Acquire);
        let scanned = RequestUsageStore::read_requests(&store.session_dir("session")?).await;
        request.unpriced_attempts = 2;
        store.save("session", &request).await?;
        assert!(!RequestUsageStore::publish_refresh(&cache, revision, scanned).await?);
        store.refresh("session").await?;
        assert_eq!(
            store.load("session").await?.get(&request.message_id),
            Some(&request)
        );
        let revision = cache.revision.load(Ordering::Acquire);
        let scanned = RequestUsageStore::read_requests(&store.session_dir("session")?).await;
        store.delete("session").await?;
        assert!(!RequestUsageStore::publish_refresh(&cache, revision, scanned).await?);
        assert!(store.load("session").await?.is_empty());
        fs::remove_dir_all(directory).await?;
        Ok(())
    }

    #[tokio::test]
    async fn busy_session_does_not_block_other_session_ledger() -> Result<()> {
        let directory =
            std::env::temp_dir().join(format!("kraai-usage-{}", ulid::Ulid::generate()));
        let store = RequestUsageStore::new(&directory);
        store.save("busy", &request()).await?;
        store.load("busy").await?;
        let cache = store.session_cache("busy").await?;
        let guard = cache.io.lock().await;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            assert_eq!(store.load("busy").await?.len(), 1);
            store.save("other", &request()).await?;
            store.refresh("other").await?;
            assert_eq!(store.load("other").await?.len(), 1);
            Ok::<_, color_eyre::Report>(())
        })
        .await??;
        drop(guard);
        fs::remove_dir_all(directory).await?;
        Ok(())
    }
}
