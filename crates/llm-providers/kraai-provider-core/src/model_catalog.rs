use std::path::PathBuf;

use rusqlite::Connection;
use tokio::sync::{Mutex, MutexGuard, watch};

mod cost;
mod resolution;
mod source;
mod store;

pub use cost::CatalogCost;
pub(crate) use cost::parse_rates;
use resolution::Entry;

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_AGE: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogModelMetadata {
    pub name: Option<String>,
    pub max_context: Option<usize>,
    pub max_output: Option<usize>,
    pub supports_images: Option<bool>,
    pub options: Vec<kraai_types::ModelOptionDefinition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatalogPricingSource {
    Manufacturer,
    Provider,
}

#[derive(Default)]
struct State {
    reader: Option<Connection>,
    meta: Option<store::Meta>,
}

pub struct ModelCatalog {
    path: Option<PathBuf>,
    state: Mutex<State>,
    initialized: tokio::sync::OnceCell<()>,
    revision: watch::Sender<u64>,
}

impl Default for ModelCatalog {
    fn default() -> Self {
        Self::new(store::cache_path())
    }
}

pub struct ModelCatalogView<'a> {
    state: MutexGuard<'a, State>,
    revision: u64,
}

impl ModelCatalog {
    pub(crate) fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            state: Mutex::default(),
            initialized: tokio::sync::OnceCell::default(),
            revision: watch::channel(0).0,
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.revision.subscribe()
    }

    pub fn revision(&self) -> u64 {
        *self.revision.borrow()
    }

    pub async fn view(&self) -> ModelCatalogView<'_> {
        let mut state = self.state.lock().await;
        let snapshot = state.reader.as_ref().map(|reader| {
            reader.execute_batch("BEGIN")?;
            store::meta(reader)
        });
        match snapshot {
            Some(Ok(Some(meta))) => self.record(&mut state, Some(meta)),
            Some(Err(error)) => tracing::debug!(%error, "Could not start a model catalog read"),
            Some(Ok(None)) | None => {}
        }
        ModelCatalogView {
            state,
            revision: self.revision(),
        }
    }

    pub async fn initialize(&self) {
        self.initialized
            .get_or_init(|| async {
                self.observe().await;
            })
            .await;
    }

    pub async fn refresh(&self) -> bool {
        let fresh = |meta: Option<store::Meta>| {
            meta.is_some_and(|meta| is_fresh(meta.fetched_at, crate::pricing::now()))
        };
        if fresh(self.observe().await) {
            return true;
        }
        if let Err(error) = self.fetch().await {
            tracing::debug!(%error, "Could not refresh model catalog; retaining cached metadata");
        }
        fresh(self.state.lock().await.meta)
    }

    async fn observe(&self) -> Option<store::Meta> {
        let mut state = self.state.lock().await;
        let meta = match state.reader.as_ref().map(store::meta) {
            Some(Ok(Some(meta))) => Some(meta),
            Some(Ok(None) | Err(_)) | None => {
                let path = self.path.clone();
                let opened =
                    tokio::task::spawn_blocking(move || path.as_deref().and_then(store::open))
                        .await
                        .ok()
                        .flatten();
                let (reader, meta) = opened.unzip();
                state.reader = reader;
                meta
            }
        };
        self.record(&mut state, meta);
        drop(state);
        meta
    }

    fn record(&self, state: &mut State, meta: Option<store::Meta>) {
        if let Some(meta) = meta
            && state.meta.map(|known| known.generation) != Some(meta.generation)
        {
            self.revision
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
        state.meta = meta;
    }

    async fn fetch(&self) -> color_eyre::Result<()> {
        let client = kraai_io::http::build_finite_http_client()?;
        let response = client
            .get("https://models.dev/api.json")
            .header(reqwest::header::USER_AGENT, "kraai/0.1")
            .send()
            .await?
            .error_for_status()?;
        let bytes = kraai_io::http::read_response_body(response, MAX_BYTES)
            .await
            .map_err(|error| match error {
                kraai_io::http::BodyReadError::TooLarge { .. } => {
                    color_eyre::eyre::eyre!("Model catalog exceeds size limit")
                }
                kraai_io::http::BodyReadError::Transport(error) => error.into(),
            })?;
        self.install(bytes, crate::pricing::now()).await
    }

    async fn install(&self, bytes: Vec<u8>, fetched_at: u64) -> color_eyre::Result<()> {
        let path = self.path.clone();
        let (reader, meta) =
            tokio::task::spawn_blocking(move || store::write(path.as_deref(), &bytes, fetched_at))
                .await??;
        let mut state = self.state.lock().await;
        let previous = state.reader.replace(reader);
        self.record(&mut state, Some(meta));
        drop(state);
        drop(previous);
        Ok(())
    }

    pub(crate) async fn lookup(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
        source: CatalogPricingSource,
    ) -> Option<(CatalogCost, String, u64)> {
        self.view().await.lookup(provider, api, model, source)
    }

    pub async fn metadata(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
    ) -> Option<CatalogModelMetadata> {
        self.metadata_with_protocol(provider, api, model, None)
            .await
    }

    pub async fn metadata_for_protocol(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
        protocol: crate::ModelOptionsProtocol,
    ) -> Option<CatalogModelMetadata> {
        self.metadata_with_protocol(provider, api, model, Some(protocol))
            .await
    }

    async fn metadata_with_protocol(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
        protocol: Option<crate::ModelOptionsProtocol>,
    ) -> Option<CatalogModelMetadata> {
        let (mut metadata, discovery) = self
            .metadata_with_discovery(provider, api, model, None)
            .await?;
        if let Some(protocol) = protocol {
            metadata.options = discovery.definitions(protocol);
        }
        Some(metadata)
    }

    pub async fn metadata_with_discovery(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
        owner: Option<&str>,
    ) -> Option<(CatalogModelMetadata, crate::DiscoveredModelOptions)> {
        self.view()
            .await
            .metadata_with_discovery(provider, api, model, owner)
    }
}

impl ModelCatalogView<'_> {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn lookup(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
        source: CatalogPricingSource,
    ) -> Option<(CatalogCost, String, u64)> {
        let connection = self.state.reader.as_ref()?;
        reported(lookup(connection, provider, api, model, source))
    }

    pub fn metadata_with_discovery(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
        owner: Option<&str>,
    ) -> Option<(CatalogModelMetadata, crate::DiscoveredModelOptions)> {
        let connection = self.state.reader.as_ref()?;
        reported(metadata_with_discovery(
            connection, provider, api, model, owner,
        ))
    }
}

impl Drop for ModelCatalogView<'_> {
    fn drop(&mut self) {
        if let Some(reader) = &self.state.reader
            && !reader.is_autocommit()
            && let Err(error) = reader
                .execute_batch("COMMIT")
                .or_else(|_| reader.execute_batch("ROLLBACK"))
        {
            tracing::debug!(%error, "Could not finish a model catalog read");
        }
    }
}

fn is_fresh(fetched_at: u64, now: u64) -> bool {
    fetched_at <= now && now - fetched_at < MAX_AGE
}

fn reported<T>(result: rusqlite::Result<Option<T>>) -> Option<T> {
    result
        .inspect_err(|error| tracing::debug!(%error, "Could not read model catalog"))
        .ok()
        .flatten()
}

fn metadata_with_discovery(
    connection: &Connection,
    provider: Option<&str>,
    api: Option<&str>,
    model: &str,
    owner: Option<&str>,
) -> rusqlite::Result<Option<(CatalogModelMetadata, crate::DiscoveredModelOptions)>> {
    let serving = resolution::serving_provider(connection, provider, api)?;
    let served = resolution::serving_model(connection, &serving, model)?;
    let owned = match served {
        Some(_) => None,
        None => resolution::owned_model(connection, &serving, model, owner)?,
    };
    let discovery = match (&served, &owned) {
        (Some(entry), _) => entry.options(),
        (None, Some(entry)) => entry.options().reasoning_only(),
        (None, None) => crate::DiscoveredModelOptions::default(),
    };
    let entry = match served.or(owned) {
        Some(entry) => entry,
        None => match resolution::model(connection, &serving, model)? {
            Some(entry) => entry,
            None => return Ok(None),
        },
    };
    let Entry {
        name,
        max_context,
        max_output,
        supports_images,
        ..
    } = entry;
    let metadata = CatalogModelMetadata {
        name,
        max_context,
        max_output,
        supports_images,
        options: Vec::new(),
    };
    Ok(Some((metadata, discovery)))
}

fn lookup(
    connection: &Connection,
    provider: Option<&str>,
    api: Option<&str>,
    model: &str,
    source: CatalogPricingSource,
) -> rusqlite::Result<Option<(CatalogCost, String, u64)>> {
    let serving = resolution::serving_provider(connection, provider, api)?;
    let Some(resolved) = resolution::model(connection, &serving, model)? else {
        return Ok(None);
    };
    let entry = match (source, &resolved.canonical_model_id) {
        (CatalogPricingSource::Manufacturer, Some(canonical)) => {
            match resolution::canonical_model(connection, canonical)? {
                Some(entry) => entry,
                None => return Ok(None),
            }
        }
        (CatalogPricingSource::Manufacturer, None) | (CatalogPricingSource::Provider, _) => {
            resolved
        }
    };
    let (Some(cost), Some(fetched_at)) = (
        entry.cost(connection)?,
        store::meta(connection)?.map(|meta| meta.fetched_at),
    ) else {
        return Ok(None);
    };
    Ok(Some((
        cost,
        format!("models.dev/{}/{}", entry.provider, entry.model),
        fetched_at,
    )))
}

#[cfg(test)]
mod tests;
