use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Result, eyre};
use kraai_persistence::{ModelListVersion, SqliteModelListStore};
use kraai_provider_core::{Provider, ProviderManager, ProviderModelCacheRefreshError};
use kraai_types::ProviderId;
use tokio::task::JoinHandle;

use super::core::RuntimeCore;
use crate::Event;

const MAX_AGE: Duration = Duration::from_secs(10 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const CLAIM_TTL: Duration = Duration::from_secs(2 * 60);
const SYNC_CONCURRENCY: usize = 8;

/// Keeps this process's provider model lists in step with the lists shared through the database.
pub(crate) struct ModelListSync {
    store: SqliteModelListStore,
    state: Mutex<SyncState>,
}

#[derive(Default)]
struct SyncState {
    installed: BTreeMap<ProviderId, ModelListVersion>,
    failed_claims: BTreeMap<ProviderId, i64>,
}

pub(crate) struct SyncOutcome {
    pub(crate) changed: bool,
    pub(crate) failures: Vec<ProviderModelCacheRefreshError>,
}

#[derive(Default)]
struct ProviderSync {
    installed: Option<ModelListVersion>,
    failed_claim: Option<i64>,
    failure: Option<String>,
}

impl ModelListSync {
    pub(crate) fn new(store: SqliteModelListStore) -> Self {
        Self {
            store,
            state: Mutex::default(),
        }
    }

    fn state(&self) -> MutexGuard<'_, SyncState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Forgets what was installed, for freshly created providers, and gives up the claims
    /// this process kept after failed fetches so the new configuration retries at once.
    pub(crate) async fn reset(&self) {
        let failed_claims = std::mem::take(&mut *self.state()).failed_claims;
        for (provider, claimed_until) in failed_claims {
            if let Err(error) = self.store.release(&provider, claimed_until).await {
                tracing::warn!(%provider, error = %format!("{error:#}"), "Unable to release model list claim");
            }
        }
    }

    pub(crate) async fn sync(&self, providers: &ProviderManager, now: i64) -> Result<SyncOutcome> {
        let stored = self.store.versions().await?;
        let installed = self.state().installed.clone();
        let mut pending = providers.providers().filter_map(|(id, provider)| {
            let source = provider.model_list_source()?;
            Some(ProviderSyncTask {
                store: self.store.clone(),
                id: id.clone(),
                provider: provider.clone(),
                source,
                stored: stored.get(id).cloned(),
                installed: installed.get(id).cloned(),
                now,
            })
        });
        let mut tasks = tokio::task::JoinSet::new();
        let mut outcome = SyncOutcome {
            changed: false,
            failures: Vec::new(),
        };
        loop {
            while tasks.len() < SYNC_CONCURRENCY {
                let Some(task) = pending.next() else {
                    break;
                };
                tasks.spawn(task.run());
            }
            let Some(joined) = tasks.join_next().await else {
                break;
            };
            let (provider_id, result) = match joined {
                Ok(joined) => joined,
                Err(error) => {
                    outcome.failures.push(ProviderModelCacheRefreshError {
                        provider_id: ProviderId::new("<unknown>"),
                        message: format!("Model list sync task failed: {error}"),
                    });
                    continue;
                }
            };
            let mut state = self.state();
            if let Some(version) = result.installed {
                outcome.changed = true;
                state.installed.insert(provider_id.clone(), version);
                state.failed_claims.remove(&provider_id);
            }
            if let Some(claimed_until) = result.failed_claim {
                state
                    .failed_claims
                    .insert(provider_id.clone(), claimed_until);
            }
            drop(state);
            if let Some(message) = result.failure {
                outcome.failures.push(ProviderModelCacheRefreshError {
                    provider_id,
                    message,
                });
            }
        }
        Ok(outcome)
    }
}

struct ProviderSyncTask {
    store: SqliteModelListStore,
    id: ProviderId,
    provider: Arc<dyn Provider>,
    source: String,
    stored: Option<ModelListVersion>,
    installed: Option<ModelListVersion>,
    now: i64,
}

impl ProviderSyncTask {
    async fn run(self) -> (ProviderId, ProviderSync) {
        let mut result = ProviderSync::default();
        let mut current = self
            .installed
            .clone()
            .filter(|version| version.source == self.source);
        let mut seen = self.stored.clone();
        if let Some(stored) = &self.stored
            && stored.source == self.source
            && current
                .as_ref()
                .is_none_or(|current| stored.fetched_at > current.fetched_at)
        {
            match self.install_stored().await {
                Ok(version) => {
                    seen = Some(version.clone());
                    current = Some(version.clone());
                    result.installed = Some(version);
                }
                Err(error) => {
                    tracing::warn!(id = %self.id, error = %format!("{error:#}"), "Ignoring unusable stored model list");
                }
            }
        }

        let max_age = i64::try_from(MAX_AGE.as_millis()).unwrap_or(i64::MAX);
        if current.is_some_and(|version| self.now.saturating_sub(version.fetched_at) < max_age) {
            return (self.id, result);
        }
        let claimed_until = self
            .now
            .saturating_add(i64::try_from(CLAIM_TTL.as_millis()).unwrap_or(i64::MAX));
        match self
            .store
            .claim(&self.id, seen.as_ref(), self.now, claimed_until)
            .await
        {
            Ok(true) => {}
            Ok(false) => return (self.id, result),
            Err(error) => {
                result.failure = Some(format!("{error:#}"));
                return (self.id, result);
            }
        }
        let fetched = match self.provider.fetch_model_list().await {
            Ok(list) => self
                .provider
                .install_model_list(list.clone())
                .await
                .map(|()| list),
            Err(error) => Err(error),
        };
        let list = match fetched {
            Ok(list) => list,
            Err(error) => {
                result.failed_claim = Some(claimed_until);
                result.failure = Some(format!("{error:#}"));
                return (self.id, result);
            }
        };
        let version = ModelListVersion {
            source: self.source,
            fetched_at: self.now,
        };
        if let Err(error) = self.store.save(&self.id, &version, &list).await {
            result.failed_claim = Some(claimed_until);
            result.failure = Some(format!("{error:#}"));
        }
        result.installed = Some(version);
        (self.id, result)
    }

    async fn install_stored(&self) -> Result<ModelListVersion> {
        let stored = self
            .store
            .read(&self.id)
            .await?
            .filter(|stored| stored.version.source == self.source)
            .ok_or_else(|| eyre!("Stored model list changed source"))?;
        self.provider.install_model_list(stored.data).await?;
        Ok(stored.version)
    }
}

impl RuntimeCore {
    pub(crate) async fn sync_model_lists(&self, now: i64) -> Result<SyncOutcome> {
        self.model_lists
            .sync(&self.agent_manager.cloned_provider_manager(), now)
            .await
    }

    pub(crate) async fn poll_model_lists(&self, now: i64) {
        let reload = self.config_reload.lock().await;
        let outcome = self.sync_model_lists(now).await;
        drop(reload);
        match outcome {
            Ok(outcome) => {
                for failure in &outcome.failures {
                    tracing::warn!(provider = %failure.provider_id, error = %failure.message, "Model list refresh failed");
                }
                if outcome.changed {
                    self.send_event(Event::ModelsUpdated);
                }
            }
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "Model list sync failed");
            }
        }
    }

    pub(crate) fn spawn_model_list_sync(&self) -> JoinHandle<()> {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval_at(
                tokio::time::Instant::now() + POLL_INTERVAL,
                POLL_INTERVAL,
            );
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                runtime.poll_model_lists(unix_millis()).await;
            }
        })
    }
}

pub(crate) fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}
