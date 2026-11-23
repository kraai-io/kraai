use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Result, eyre};
use kraai_persistence::{
    LEASE_DURATION, LEASE_RENEWAL_INTERVAL, ModelListVersion, SqliteModelListStore,
};
use kraai_provider_core::{
    Provider, ProviderError, ProviderManager, ProviderModelCacheRefreshError,
};
use kraai_types::ProviderId;
use tokio::sync::Notify;
use tokio::task::{JoinHandle, JoinSet};

use super::core::RuntimeCore;
use crate::Event;

pub(crate) const MAX_AGE: Duration = Duration::from_secs(10 * 60);
pub(crate) const MANUAL_MAX_AGE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const FAILURE_BACKOFF: Duration = Duration::from_secs(2 * 60);
const SYNC_CONCURRENCY: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refetch {
    Never,
    OlderThan(Duration),
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SyncRequest {
    max_age: Duration,
    report_failures: bool,
}

impl SyncRequest {
    pub(crate) const POLL: Self = Self {
        max_age: MAX_AGE,
        report_failures: false,
    };
    pub(crate) const REFRESH: Self = Self {
        max_age: MANUAL_MAX_AGE,
        report_failures: false,
    };
    pub(crate) const CONFIG_LOADED: Self = Self {
        max_age: MAX_AGE,
        report_failures: true,
    };

    fn merge(self, other: Self) -> Self {
        Self {
            max_age: self.max_age.min(other.max_age),
            report_failures: self.report_failures || other.report_failures,
        }
    }
}

pub(crate) struct ModelListSync {
    store: SqliteModelListStore,
    state: Mutex<SyncState>,
    requested: Notify,
    refetches: Mutex<JoinSet<()>>,
}

#[derive(Default)]
struct SyncState {
    installed: BTreeMap<ProviderId, ModelListVersion>,
    backoff_until: BTreeMap<ProviderId, i64>,
    generation: u64,
    requested: Option<SyncRequest>,
}

pub(crate) struct SyncOutcome {
    pub(crate) changed: bool,
    pub(crate) failures: Vec<ProviderModelCacheRefreshError>,
}

#[derive(Default)]
struct ProviderSync {
    installed: Option<ModelListVersion>,
    backoff_until: Option<i64>,
    failure: Option<String>,
}

impl ModelListSync {
    pub(crate) fn new(store: SqliteModelListStore) -> Self {
        Self {
            store,
            state: Mutex::default(),
            requested: Notify::new(),
            refetches: Mutex::default(),
        }
    }

    fn state(&self) -> MutexGuard<'_, SyncState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn refetches(&self) -> MutexGuard<'_, JoinSet<()>> {
        self.refetches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn reset(&self) {
        let mut state = self.state();
        state.installed.clear();
        state.backoff_until.clear();
        state.generation = state.generation.wrapping_add(1);
    }

    pub(crate) fn request_sync(&self, request: SyncRequest) {
        let mut state = self.state();
        state.requested = Some(
            state
                .requested
                .map_or(request, |requested| requested.merge(request)),
        );
        drop(state);
        self.requested.notify_one();
    }

    fn take_request(&self) -> Option<SyncRequest> {
        self.state().requested.take()
    }

    pub(crate) async fn sync(
        &self,
        providers: &ProviderManager,
        only: Option<&ProviderId>,
        now: i64,
        refetch: Refetch,
    ) -> Result<SyncOutcome> {
        let stored = self.store.versions().await?;
        let (installed, backoff_until, generation) = {
            let state = self.state();
            (
                state.installed.clone(),
                state.backoff_until.clone(),
                state.generation,
            )
        };
        let mut pending = providers.providers().filter_map(|(id, provider)| {
            if only.is_some_and(|only| only != id) {
                return None;
            }
            let source = provider.model_list_source()?;
            Some(ProviderSyncTask {
                store: self.store.clone(),
                id: id.clone(),
                provider: provider.clone(),
                source,
                stored: stored.get(id).cloned(),
                installed: installed.get(id).cloned(),
                backoff_until: backoff_until.get(id).copied(),
                now,
                refetch,
            })
        });
        let mut tasks = JoinSet::new();
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
                if state.generation == generation {
                    state.installed.insert(provider_id.clone(), version);
                    state.backoff_until.remove(&provider_id);
                }
            }
            if let Some(until) = result.backoff_until
                && state.generation == generation
            {
                state.backoff_until.insert(provider_id.clone(), until);
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
    backoff_until: Option<i64>,
    now: i64,
    refetch: Refetch,
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

        let due = match self.refetch {
            Refetch::Never => false,
            Refetch::Always => true,
            Refetch::OlderThan(max_age) => {
                let max_age = i64::try_from(max_age.as_millis()).unwrap_or(i64::MAX);
                self.backoff_until.is_none_or(|until| self.now >= until)
                    && current.is_none_or(|version| {
                        self.now.saturating_sub(version.fetched_at) >= max_age
                    })
            }
        };
        if !due {
            return (self.id, result);
        }
        let now = unix_millis().max(self.now);
        let claimed_until = now.saturating_add(millis(LEASE_DURATION));
        let claimed = if self.refetch == Refetch::Always {
            self.store
                .claim_any_version(&self.id, now, claimed_until)
                .await
        } else {
            self.store
                .claim(&self.id, seen.as_ref(), now, claimed_until)
                .await
        };
        match claimed {
            Ok(true) => {}
            Ok(false) => return (self.id, result),
            Err(error) => {
                result.failure = Some(format!("{error:#}"));
                return (self.id, result);
            }
        }
        let Some((fetched, claimed_until)) = self.fetch_holding_claim(claimed_until).await else {
            tracing::warn!(id = %self.id, "Lost the model list claim to another fetch");
            return (self.id, result);
        };
        let fetched = match fetched {
            Ok(list) => self
                .provider
                .install_model_list(list.clone())
                .await
                .map(|()| list),
            Err(error) => Err(error),
        };
        let saved = match fetched {
            Ok(list) => {
                let version = ModelListVersion {
                    source: self.source.clone(),
                    fetched_at: unix_millis().max(self.now),
                };
                match self
                    .store
                    .save(&self.id, claimed_until, &version, &list)
                    .await
                {
                    Ok(Some(saved)) => {
                        result.installed = Some(saved);
                        Ok(())
                    }
                    Ok(None) => {
                        tracing::warn!(id = %self.id, "Lost the model list claim before saving");
                        Ok(())
                    }
                    Err(error) => {
                        result.installed = Some(version);
                        Err(error)
                    }
                }
            }
            Err(error) => Err(error),
        };
        if let Err(error) = self.store.release(&self.id, claimed_until).await {
            tracing::warn!(id = %self.id, error = %format!("{error:#}"), "Unable to release model list claim");
        }
        if let Err(error) = saved {
            result.backoff_until = Some(self.now.saturating_add(millis(FAILURE_BACKOFF)));
            result.failure = Some(format!("{error:#}"));
        }
        (self.id, result)
    }

    async fn fetch_holding_claim(
        &self,
        mut claimed_until: i64,
    ) -> Option<(Result<serde_json::Value>, i64)> {
        let fetch = self.provider.fetch_model_list();
        tokio::pin!(fetch);
        let mut renewal = tokio::time::interval_at(
            tokio::time::Instant::now() + LEASE_RENEWAL_INTERVAL,
            LEASE_RENEWAL_INTERVAL,
        );
        renewal.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                fetched = &mut fetch => return Some((fetched, claimed_until)),
                _ = renewal.tick() => {
                    let until = unix_millis()
                        .max(self.now)
                        .saturating_add(millis(LEASE_DURATION));
                    match self.store.renew(&self.id, claimed_until, until).await {
                        Ok(true) => claimed_until = until,
                        Ok(false) => return None,
                        Err(error) => {
                            tracing::warn!(id = %self.id, error = %format!("{error:#}"), "Unable to renew model list claim");
                        }
                    }
                }
            }
        }
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
    pub(crate) async fn sync_model_lists(&self, now: i64, refetch: Refetch) -> Result<SyncOutcome> {
        self.model_lists
            .sync(
                &self.agent_manager.cloned_provider_manager(),
                None,
                now,
                refetch,
            )
            .await
    }

    pub(crate) async fn poll_model_lists(&self, now: i64, request: SyncRequest) {
        let reload = self.config_reload.lock().await;
        let outcome = self
            .sync_model_lists(now, Refetch::OlderThan(request.max_age))
            .await;
        drop(reload);
        self.report_model_list_sync(outcome, request.report_failures);
    }

    pub(crate) fn spawn_model_list_refetch(&self, provider: ProviderId) {
        let mut refetches = self.model_lists.refetches();
        if self.is_stopping() {
            return;
        }
        while refetches.try_join_next().is_some() {}
        let runtime = self.clone();
        refetches.spawn(async move { runtime.refetch_model_list(&provider).await });
    }

    pub(crate) async fn stop_model_list_refetches(&self) {
        let mut refetches = std::mem::take(&mut *self.model_lists.refetches());
        refetches.shutdown().await;
    }

    pub(crate) async fn refetch_model_list(&self, provider: &ProviderId) {
        let outcome = self
            .model_lists
            .sync(
                &self.agent_manager.cloned_provider_manager(),
                Some(provider),
                unix_millis(),
                Refetch::Always,
            )
            .await;
        self.report_model_list_sync(outcome, true);
    }

    fn report_model_list_sync(&self, outcome: Result<SyncOutcome>, report_failures: bool) {
        match outcome {
            Ok(outcome) => {
                if outcome.changed {
                    self.send_event(Event::ModelsUpdated);
                }
                if report_failures && !outcome.failures.is_empty() {
                    self.send_service_error(ProviderError::ModelCacheRefreshFailed(
                        outcome.failures,
                    ));
                } else {
                    for failure in &outcome.failures {
                        tracing::warn!(provider = %failure.provider_id, error = %failure.message, "Model list refresh failed");
                    }
                }
            }
            Err(error) if report_failures => self.send_service_error(format!("{error:#}")),
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
                let request = tokio::select! {
                    _ = interval.tick() => Some(SyncRequest::POLL),
                    () = runtime.model_lists.requested.notified() => None,
                };
                let requested = runtime.model_lists.take_request();
                let Some(request) = request
                    .into_iter()
                    .chain(requested)
                    .reduce(SyncRequest::merge)
                else {
                    continue;
                };
                runtime.poll_model_lists(unix_millis(), request).await;
            }
        })
    }
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

pub(crate) fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}
