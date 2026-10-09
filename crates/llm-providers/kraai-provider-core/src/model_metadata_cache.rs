use std::collections::BTreeMap;

use color_eyre::{Result, eyre::eyre};
use kraai_types::ModelId;
use tokio::sync::{Mutex, RwLock, RwLockReadGuard};

use crate::{Model, ModelCatalog, ModelCatalogView};

pub struct ModelMetadataCache<T> {
    state: RwLock<CacheState<T>>,
    refresh: Mutex<()>,
}

struct CacheState<T> {
    native: T,
    models: BTreeMap<ModelId, Model>,
    revision: Option<u64>,
}

impl<T: Default> Default for ModelMetadataCache<T> {
    fn default() -> Self {
        Self {
            state: RwLock::new(CacheState {
                native: T::default(),
                models: BTreeMap::new(),
                revision: None,
            }),
            refresh: Mutex::new(()),
        }
    }
}

impl<T: Send + Sync> ModelMetadataCache<T> {
    pub fn invalidate(&mut self) {
        self.state.get_mut().revision = None;
    }

    pub async fn refresh(
        &self,
        catalog: Option<&ModelCatalog>,
        discover: impl Future<Output = Result<T>> + Send,
        resolve: impl FnOnce(&T, Option<&ModelCatalogView<'_>>) -> BTreeMap<ModelId, Model> + Send,
    ) -> Result<()> {
        let refresh = self.refresh.lock().await;
        let native = discover.await?;
        let mut state = self.state.write().await;
        let view = match catalog {
            Some(catalog) => Some(catalog.view().await),
            None => None,
        };
        let models = resolve(&native, view.as_ref());
        validate_models(&models)?;
        let revision = Some(view.as_ref().map_or(0, ModelCatalogView::revision));
        drop(view);
        let previous = std::mem::replace(
            &mut *state,
            CacheState {
                native,
                models,
                revision,
            },
        );
        drop(state);
        drop(refresh);
        drop(previous);
        Ok(())
    }

    pub async fn models(
        &self,
        catalog: Option<&ModelCatalog>,
        resolve: impl FnOnce(&T, Option<&ModelCatalogView<'_>>) -> BTreeMap<ModelId, Model> + Send,
    ) -> RwLockReadGuard<'_, BTreeMap<ModelId, Model>> {
        let revision = Some(catalog.map_or(0, ModelCatalog::revision));
        let state = self.state.read().await;
        if state.revision == revision {
            return RwLockReadGuard::map(state, |state| &state.models);
        }
        drop(state);
        let mut state = self.state.write().await;
        let view = match catalog {
            Some(catalog) => Some(catalog.view().await),
            None => None,
        };
        let revision = Some(view.as_ref().map_or(0, ModelCatalogView::revision));
        let previous = if state.revision != revision {
            let models = resolve(&state.native, view.as_ref());
            state.revision = revision;
            match validate_models(&models) {
                Ok(()) => Some(std::mem::replace(&mut state.models, models)),
                Err(error) => {
                    tracing::warn!(%error, "Could not update model metadata; retaining cached models");
                    None
                }
            }
        } else {
            None
        };
        drop(view);
        let state = state.downgrade();
        drop(previous);
        RwLockReadGuard::map(state, |state| &state.models)
    }
}

fn validate_models(models: &BTreeMap<ModelId, Model>) -> Result<()> {
    for (id, model) in models {
        kraai_types::validate_model_option_values(&model.options, &Default::default(), false)
            .map_err(|errors| eyre!("Invalid model options for {id}: {errors:?}"))?;
    }
    Ok(())
}
