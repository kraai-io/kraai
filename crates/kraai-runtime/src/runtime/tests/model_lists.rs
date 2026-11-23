use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use color_eyre::eyre::{Result, bail};
use futures::stream::BoxStream;
use kraai_provider_core::{
    Model, ModelConfig, Provider, ProviderManager, ProviderRequest, ProviderRequestContext,
    ProviderStreamEvent,
};
use kraai_types::{ModelId, ProviderId};
use serde_json::json;

use super::harness::RuntimeTestHarness;
use crate::Event;
use crate::runtime::model_lists::{MANUAL_MAX_AGE, MAX_AGE, Refetch, SyncRequest, unix_millis};

const STALE: i64 = 11 * 60 * 1000;

struct Unlisted {
    runtime: crate::RuntimeHandle,
    root: PathBuf,
    _server: tokio_util::task::AbortOnDropHandle<()>,
}

impl Unlisted {
    async fn start(respond: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut held = Vec::new();
            while let Ok((mut stream, _)) = listener.accept().await {
                if !respond {
                    held.push(stream);
                    continue;
                }
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
            }
        }));
        let root = std::env::temp_dir().join(format!(
            "kraai-runtime-unlisted-{respond}-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let config = root.join("providers.toml");
        tokio::fs::write(
            &config,
            format!(
                "[[provider]]\nid = \"unlisted\"\ntype = \"openai-chat-completions\"\nbase_url = \"http://{address}\"\napi_key = \"test\"\n\n[[model]]\nid = \"configured\"\nprovider_id = \"unlisted\"\n"
            ),
        )
        .await
        .unwrap();
        let runtime = crate::RuntimeBuilder::new(kraai_providers::DefaultProviders)
            .storage_root(root.join("state"))
            .provider_config_path(config)
            .mcp_config_path(root.join("mcp.toml"))
            .build_on(&tokio::runtime::Handle::current());
        Self {
            runtime,
            root,
            _server: server,
        }
    }

    async fn configured_models(&self) -> Vec<String> {
        self.runtime
            .list_models()
            .await
            .unwrap()
            .remove("unlisted")
            .expect("unlisted provider is missing")
            .into_iter()
            .map(|model| model.id)
            .collect()
    }

    async fn shutdown(self) {
        self.runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(&self.root).await;
    }
}

#[tokio::test]
async fn startup_succeeds_when_a_provider_cannot_list_its_models() {
    let unlisted = Unlisted::start(true).await;
    let mut events = unlisted.runtime.subscribe();

    assert_eq!(
        unlisted.runtime.wait_for_startup().await.unwrap(),
        crate::RuntimeStartupState::ready()
    );
    assert_eq!(unlisted.configured_models().await, ["configured"]);
    let reported = loop {
        if let Event::ServiceError { error } = events.recv().await.unwrap().event {
            break error.to_string();
        }
    };
    assert!(reported.contains("unlisted"), "{reported}");
    unlisted.shutdown().await
}

#[tokio::test]
async fn startup_does_not_wait_for_a_provider_that_never_answers() {
    let unlisted = Unlisted::start(false).await;
    let startup = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        unlisted.runtime.wait_for_startup(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(startup, crate::RuntimeStartupState::ready());
    assert_eq!(unlisted.configured_models().await, ["configured"]);
    unlisted.shutdown().await
}

#[derive(Clone, Default)]
struct Upstream {
    lists: Arc<AtomicUsize>,
    failing: Arc<AtomicBool>,
    gate: Arc<tokio::sync::Mutex<()>>,
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
                max_output: None,
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
        drop(self.upstream.gate.lock().await);
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
    async fn start(upstream: &Upstream, directory: Option<PathBuf>) -> Self {
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
        let harness = RuntimeTestHarness::new_in_directory(providers, None, directory).await;
        Self { harness, attempts }
    }

    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    async fn listed(&self) -> Vec<String> {
        self.harness
            .handle
            .list_models()
            .await
            .unwrap()
            .remove("listed")
            .expect("listed provider is missing")
            .into_iter()
            .map(|model| model.id)
            .collect()
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
async fn a_second_process_installs_the_stored_list_without_fetching() {
    let upstream = Upstream::default();
    let first = Process::start(&upstream, None).await;
    first
        .harness
        .runtime
        .poll_model_lists(unix_millis(), SyncRequest::POLL)
        .await;
    assert_eq!(first.attempts(), 1);
    assert_eq!(first.listed().await, ["model-1"]);
    first.wait_for_models_updates(1).await;

    let second = Process::start(&upstream, Some(first.harness.data_dir.clone())).await;
    second
        .harness
        .runtime
        .poll_model_lists(unix_millis(), SyncRequest::POLL)
        .await;
    assert_eq!(second.attempts(), 0);
    assert_eq!(second.listed().await, ["model-1"]);
    second.wait_for_models_updates(1).await;

    second
        .harness
        .runtime
        .poll_model_lists(unix_millis(), SyncRequest::POLL)
        .await;
    first
        .harness
        .runtime
        .poll_model_lists(unix_millis(), SyncRequest::POLL)
        .await;
    assert_eq!(first.attempts() + second.attempts(), 1);
    assert_eq!(second.models_updates(), 1);
    second.harness.shutdown().await;
    first.harness.shutdown().await;
}

#[tokio::test]
async fn a_stale_list_is_refetched_by_one_process_and_picked_up_by_the_other() {
    let upstream = Upstream::default();
    let first = Process::start(&upstream, None).await;
    let second = Process::start(&upstream, Some(first.harness.data_dir.clone())).await;
    first
        .harness
        .runtime
        .poll_model_lists(unix_millis(), SyncRequest::POLL)
        .await;
    second
        .harness
        .runtime
        .poll_model_lists(unix_millis(), SyncRequest::POLL)
        .await;
    assert_eq!(second.listed().await, ["model-1"]);

    let later = unix_millis() + STALE;
    tokio::join!(
        first
            .harness
            .runtime
            .poll_model_lists(later, SyncRequest::POLL),
        second
            .harness
            .runtime
            .poll_model_lists(later, SyncRequest::POLL),
    );
    assert_eq!(first.attempts() + second.attempts(), 2);
    first
        .harness
        .runtime
        .poll_model_lists(later, SyncRequest::POLL)
        .await;
    second
        .harness
        .runtime
        .poll_model_lists(later, SyncRequest::POLL)
        .await;
    assert_eq!(first.attempts() + second.attempts(), 2);
    assert_eq!(first.listed().await, ["model-2"]);
    assert_eq!(second.listed().await, ["model-2"]);
    first.wait_for_models_updates(2).await;
    second.wait_for_models_updates(2).await;
    second.harness.shutdown().await;
    first.harness.shutdown().await;
}

#[tokio::test]
async fn a_stored_list_older_than_the_installed_one_is_ignored() {
    let upstream = Upstream::default();
    let process = Process::start(&upstream, None).await;
    let runtime = &process.harness.runtime;
    runtime
        .sync_model_lists(unix_millis(), Refetch::OlderThan(MAX_AGE))
        .await
        .unwrap();
    rusqlite::Connection::open(process.harness.data_dir.join("kraai.sqlite3")).unwrap().execute(
        "UPDATE model_lists SET fetched_at = fetched_at - 60 * 1000, data = '{\"models\":[\"older\"]}'",
        [],
    ).unwrap();

    let outcome = runtime
        .sync_model_lists(unix_millis(), Refetch::OlderThan(MAX_AGE))
        .await
        .unwrap();
    assert!(!outcome.changed && outcome.failures.is_empty());
    assert_eq!(process.attempts(), 1);
    assert_eq!(process.listed().await, ["model-1"]);
    process.harness.shutdown().await;
}

#[tokio::test]
async fn a_failed_refresh_keeps_the_installed_list_and_backs_off_until_reload() {
    let upstream = Upstream::default();
    let process = Process::start(&upstream, None).await;
    let runtime = &process.harness.runtime;
    assert!(
        runtime
            .sync_model_lists(unix_millis(), Refetch::OlderThan(MAX_AGE))
            .await
            .unwrap()
            .failures
            .is_empty()
    );
    let later = unix_millis() + STALE;
    upstream.failing.store(true, Ordering::SeqCst);

    let failed = runtime
        .sync_model_lists(later, Refetch::OlderThan(MAX_AGE))
        .await
        .unwrap();
    assert_eq!(failed.failures.len(), 1);
    assert_eq!(process.attempts(), 2);
    assert_eq!(process.listed().await, ["model-1"]);
    assert!(
        runtime
            .sync_model_lists(later, Refetch::OlderThan(MAX_AGE))
            .await
            .unwrap()
            .failures
            .is_empty()
    );
    assert_eq!(process.attempts(), 2);

    upstream.failing.store(false, Ordering::SeqCst);
    runtime.model_lists.reset();
    let recovered = runtime
        .sync_model_lists(later, Refetch::OlderThan(MAX_AGE))
        .await
        .unwrap();
    assert!(recovered.changed && recovered.failures.is_empty());
    assert_eq!(process.attempts(), 3);
    assert_eq!(process.listed().await, ["model-2"]);
    process.harness.shutdown().await;
}

#[tokio::test]
async fn a_manual_refresh_refetches_only_lists_older_than_a_minute() {
    let upstream = Upstream::default();
    let process = Process::start(&upstream, None).await;
    let runtime = &process.harness.runtime;
    let start = unix_millis();
    runtime.poll_model_lists(start, SyncRequest::POLL).await;
    assert_eq!(process.attempts(), 1);
    let manual = i64::try_from(MANUAL_MAX_AGE.as_millis()).unwrap();

    runtime
        .poll_model_lists(start + manual - 1000, SyncRequest::REFRESH)
        .await;
    assert_eq!(process.attempts(), 1);

    let later = start + manual + 1000;
    runtime.poll_model_lists(later, SyncRequest::REFRESH).await;
    assert_eq!(process.attempts(), 2);
    assert_eq!(process.listed().await, ["model-2"]);
    runtime
        .poll_model_lists(later + 1000, SyncRequest::REFRESH)
        .await;
    assert_eq!(process.attempts(), 2);
    process.harness.shutdown().await;
}

#[tokio::test]
async fn requesting_a_refresh_wakes_the_sync_loop_with_the_manual_max_age() {
    let upstream = Upstream::default();
    let process = Process::start(&upstream, None).await;
    rusqlite::Connection::open(process.harness.data_dir.join("kraai.sqlite3"))
        .unwrap()
        .execute(
            "INSERT INTO model_lists (provider_id, source, fetched_at, data, claimed_until)
         VALUES ('listed', 'https://models.test', ?1, '{\"models\":[\"stored\"]}', 0)",
            [unix_millis() - 2 * i64::try_from(MANUAL_MAX_AGE.as_millis()).unwrap()],
        )
        .unwrap();
    let sync =
        tokio_util::task::AbortOnDropHandle::new(process.harness.runtime.spawn_model_list_sync());

    process.harness.handle.refresh_model_lists().await.unwrap();
    process.wait_for_models_updates(1).await;
    assert_eq!(process.attempts(), 1);
    assert_eq!(process.listed().await, ["model-1"]);
    drop(sync);
    process.harness.shutdown().await;
}

#[tokio::test]
async fn a_refetch_ignores_the_max_age_and_the_failure_backoff() {
    let upstream = Upstream::default();
    let process = Process::start(&upstream, None).await;
    let runtime = &process.harness.runtime;
    let listed = ProviderId::new("listed");
    runtime
        .poll_model_lists(unix_millis(), SyncRequest::POLL)
        .await;
    runtime.refetch_model_list(&listed).await;
    assert_eq!(process.attempts(), 2);
    assert_eq!(process.listed().await, ["model-2"]);

    upstream.failing.store(true, Ordering::SeqCst);
    runtime.refetch_model_list(&listed).await;
    process
        .harness
        .events
        .wait_for("refetch failure", |events| {
            events
                .iter()
                .any(|event| matches!(event, Event::ServiceError { .. }))
        })
        .await;
    let later = unix_millis() + STALE;
    runtime.poll_model_lists(later, SyncRequest::POLL).await;
    runtime.poll_model_lists(later, SyncRequest::POLL).await;
    assert_eq!(process.attempts(), 4);

    upstream.failing.store(false, Ordering::SeqCst);
    process
        .harness
        .handle
        .refetch_model_list("listed".into())
        .await
        .unwrap();
    process.wait_for_models_updates(3).await;
    assert_eq!(process.attempts(), 5);
    assert_eq!(process.listed().await, ["model-3"]);
    assert!(
        process
            .harness
            .handle
            .refetch_model_list("missing".into())
            .await
            .is_err()
    );
    process.harness.shutdown().await;
}

#[tokio::test]
async fn a_list_being_fetched_by_any_process_is_not_fetched_again() {
    let upstream = Upstream::default();
    let first = Process::start(&upstream, None).await;
    let second = Process::start(&upstream, Some(first.harness.data_dir.clone())).await;
    let listed = ProviderId::new("listed");
    let gate = upstream.gate.clone().lock_owned().await;
    let fetching = {
        let runtime = first.harness.runtime.clone();
        let listed = listed.clone();
        tokio::spawn(async move { runtime.refetch_model_list(&listed).await })
    };
    kraai_test_support::hang_guard(async {
        while first.attempts() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    kraai_test_support::hang_guard(async {
        first.harness.runtime.refetch_model_list(&listed).await;
        first
            .harness
            .runtime
            .poll_model_lists(unix_millis(), SyncRequest::POLL)
            .await;
        second.harness.runtime.refetch_model_list(&listed).await;
        second
            .harness
            .runtime
            .poll_model_lists(unix_millis(), SyncRequest::POLL)
            .await;
    })
    .await
    .unwrap();
    assert_eq!((first.attempts(), second.attempts()), (1, 0));
    drop(gate);
    fetching.await.unwrap();
    second
        .harness
        .runtime
        .poll_model_lists(unix_millis(), SyncRequest::POLL)
        .await;
    assert_eq!(second.listed().await, ["model-1"]);
    second.harness.runtime.refetch_model_list(&listed).await;
    assert_eq!((first.attempts(), second.attempts()), (1, 1));
    second.harness.shutdown().await;
    first.harness.shutdown().await;
}

#[tokio::test]
async fn a_fetch_releases_its_claim_even_when_a_newer_list_is_already_stored() {
    let upstream = Upstream::default();
    let process = Process::start(&upstream, None).await;
    let runtime = &process.harness.runtime;
    let listed = ProviderId::new("listed");
    runtime
        .poll_model_lists(unix_millis() + STALE, SyncRequest::POLL)
        .await;
    runtime.refetch_model_list(&listed).await;
    runtime.refetch_model_list(&listed).await;
    assert_eq!(process.attempts(), 3);
    process.harness.shutdown().await;
}

#[tokio::test]
async fn a_fetch_renews_its_claim_and_stops_once_another_fetch_takes_it() {
    let upstream = Upstream::default();
    let process = Process::start(&upstream, None).await;
    let gate = upstream.gate.clone().lock_owned().await;
    let fetching = {
        let runtime = process.harness.runtime.clone();
        tokio::spawn(async move {
            runtime.refetch_model_list(&ProviderId::new("listed")).await;
        })
    };
    kraai_test_support::hang_guard(async {
        while process.attempts() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let database =
        rusqlite::Connection::open(process.harness.data_dir.join("kraai.sqlite3")).unwrap();
    let claimed_until = || {
        database.query_row(
            "SELECT claimed_until FROM model_lists WHERE provider_id = 'listed'",
            [],
            |row| row.get::<_, i64>(0),
        )
    };
    let claimed = claimed_until().unwrap();
    kraai_test_support::hang_guard(async {
        while claimed_until().unwrap() == claimed {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .unwrap()
    .unwrap();

    database
        .execute(
            "UPDATE model_lists SET claimed_until = claimed_until + 1 WHERE provider_id = 'listed'",
            [],
        )
        .unwrap();
    kraai_test_support::hang_guard(fetching)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(process.attempts(), 1);
    assert!(process.listed().await.is_empty());
    drop(gate);
    process.harness.shutdown().await;
}
