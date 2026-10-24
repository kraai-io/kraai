use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use color_eyre::eyre::{Result, bail, eyre};
use futures::stream::BoxStream;
use kraai_provider_core::{
    Model, ModelConfig, Provider, ProviderManager, ProviderRequest, ProviderRequestContext,
    ProviderStreamEvent,
};
use kraai_types::{ModelId, ProviderId};
use serde_json::json;

use super::harness::RuntimeTestHarness;
use crate::Event;
use crate::runtime::model_lists::unix_millis;

/// Later than the shared lists' maximum age.
const STALE: i64 = 11 * 60 * 1000;

#[derive(Clone, Default)]
struct Upstream {
    lists: Arc<AtomicUsize>,
    failing: Arc<AtomicBool>,
}

struct ListedProvider {
    upstream: Upstream,
    attempts: Arc<AtomicUsize>,
    models: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Provider for ListedProvider {
    fn get_provider_id(&self) -> ProviderId {
        ProviderId::new("listed")
    }

    async fn list_models(&self) -> Vec<Model> {
        self.models
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|id| Model {
                id: ModelId::new(id.clone()),
                name: id.clone(),
                max_context: None,
                supports_images: false,
                options: Vec::new(),
            })
            .collect()
    }

    fn model_list_source(&self) -> Option<String> {
        Some("https://models.test".into())
    }

    async fn fetch_model_list(&self) -> Result<serde_json::Value> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.upstream.failing.load(Ordering::SeqCst) {
            bail!("upstream unavailable");
        }
        let list = self.upstream.lists.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(json!({ "models": [format!("model-{list}")] }))
    }

    async fn install_model_list(&self, list: serde_json::Value) -> Result<()> {
        let models = serde_json::from_value(list["models"].clone())?;
        *self
            .models
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = models;
        Ok(())
    }

    async fn register_model(&mut self, _: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        _: &ModelId,
        _: ProviderRequest,
        _: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

struct Process {
    harness: RuntimeTestHarness,
    attempts: Arc<AtomicUsize>,
}

impl Process {
    async fn start(upstream: &Upstream, directory: Option<PathBuf>) -> Option<Self> {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut providers = ProviderManager::new();
        providers.register_provider(
            ProviderId::new("listed"),
            Box::new(ListedProvider {
                upstream: upstream.clone(),
                attempts: attempts.clone(),
                models: Mutex::default(),
            }),
        );
        let harness = RuntimeTestHarness::new_in_directory(providers, None, directory).await?;
        Some(Self { harness, attempts })
    }

    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    async fn listed(&self) -> Result<Vec<String>> {
        Ok(self
            .harness
            .handle
            .list_models()
            .await?
            .remove("listed")
            .ok_or_else(|| eyre!("listed provider is missing"))?
            .into_iter()
            .map(|model| model.id.to_string())
            .collect())
    }

    fn models_updates(&self) -> usize {
        self.harness
            .events
            .snapshot()
            .iter()
            .filter(|event| matches!(event, Event::ModelsUpdated))
            .count()
    }

    async fn wait_for_models_updates(&self, count: usize) {
        self.harness
            .events
            .wait_for("models update", |events| {
                events
                    .iter()
                    .filter(|event| matches!(event, Event::ModelsUpdated))
                    .count()
                    >= count
            })
            .await;
    }
}

#[tokio::test]
async fn a_second_process_installs_the_stored_list_without_fetching() -> Result<()> {
    let upstream = Upstream::default();
    let Some(first) = Process::start(&upstream, None).await else {
        return Ok(());
    };
    first.harness.runtime.poll_model_lists(unix_millis()).await;
    assert_eq!(first.attempts(), 1);
    assert_eq!(first.listed().await?, ["model-1"]);
    first.wait_for_models_updates(1).await;

    let second = Process::start(&upstream, Some(first.harness.data_dir.clone()))
        .await
        .ok_or_else(|| eyre!("second process fixture"))?;
    second.harness.runtime.poll_model_lists(unix_millis()).await;
    assert_eq!(second.attempts(), 0);
    assert_eq!(second.listed().await?, ["model-1"]);
    second.wait_for_models_updates(1).await;

    second.harness.runtime.poll_model_lists(unix_millis()).await;
    first.harness.runtime.poll_model_lists(unix_millis()).await;
    assert_eq!(first.attempts() + second.attempts(), 1);
    assert_eq!(second.models_updates(), 1);
    second.harness.shutdown().await;
    first.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_stale_list_is_refetched_by_one_process_and_picked_up_by_the_other() -> Result<()> {
    let upstream = Upstream::default();
    let Some(first) = Process::start(&upstream, None).await else {
        return Ok(());
    };
    let second = Process::start(&upstream, Some(first.harness.data_dir.clone()))
        .await
        .ok_or_else(|| eyre!("second process fixture"))?;
    first.harness.runtime.poll_model_lists(unix_millis()).await;
    second.harness.runtime.poll_model_lists(unix_millis()).await;
    assert_eq!(second.listed().await?, ["model-1"]);

    let later = unix_millis() + STALE;
    tokio::join!(
        first.harness.runtime.poll_model_lists(later),
        second.harness.runtime.poll_model_lists(later),
    );
    assert_eq!(first.attempts() + second.attempts(), 2);
    first.harness.runtime.poll_model_lists(later).await;
    second.harness.runtime.poll_model_lists(later).await;
    assert_eq!(first.attempts() + second.attempts(), 2);
    assert_eq!(first.listed().await?, ["model-2"]);
    assert_eq!(second.listed().await?, ["model-2"]);
    first.wait_for_models_updates(2).await;
    second.wait_for_models_updates(2).await;
    second.harness.shutdown().await;
    first.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_stored_list_older_than_the_installed_one_is_ignored() -> Result<()> {
    let upstream = Upstream::default();
    let Some(process) = Process::start(&upstream, None).await else {
        return Ok(());
    };
    let runtime = &process.harness.runtime;
    runtime.sync_model_lists(unix_millis()).await?;
    rusqlite::Connection::open(process.harness.data_dir.join("kraai.sqlite3"))?.execute(
        "UPDATE model_lists SET fetched_at = fetched_at - 60 * 1000, data = '{\"models\":[\"older\"]}'",
        [],
    )?;

    let outcome = runtime.sync_model_lists(unix_millis()).await?;
    assert!(!outcome.changed && outcome.failures.is_empty());
    assert_eq!(process.attempts(), 1);
    assert_eq!(process.listed().await?, ["model-1"]);
    process.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_failed_refresh_keeps_the_installed_list_and_backs_off_until_reload() -> Result<()> {
    let upstream = Upstream::default();
    let Some(process) = Process::start(&upstream, None).await else {
        return Ok(());
    };
    let runtime = &process.harness.runtime;
    assert!(
        runtime
            .sync_model_lists(unix_millis())
            .await?
            .failures
            .is_empty()
    );
    let later = unix_millis() + STALE;
    upstream.failing.store(true, Ordering::SeqCst);

    let failed = runtime.sync_model_lists(later).await?;
    assert_eq!(failed.failures.len(), 1);
    assert_eq!(process.attempts(), 2);
    assert_eq!(process.listed().await?, ["model-1"]);
    assert!(runtime.sync_model_lists(later).await?.failures.is_empty());
    assert_eq!(process.attempts(), 2);

    upstream.failing.store(false, Ordering::SeqCst);
    runtime.model_lists.reset().await;
    let recovered = runtime.sync_model_lists(later).await?;
    assert!(recovered.changed && recovered.failures.is_empty());
    assert_eq!(process.attempts(), 3);
    assert_eq!(process.listed().await?, ["model-2"]);
    process.harness.shutdown().await;
    Ok(())
}
