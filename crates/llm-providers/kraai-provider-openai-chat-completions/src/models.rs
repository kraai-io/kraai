use std::collections::BTreeMap;

use kraai_provider_core::{ConfiguredModelMetadata, Model, ModelCatalogView, ModelOptionsProtocol};
use kraai_types::ModelId;

use crate::wire::ListModelEntry;

pub(crate) struct ModelResolver<'a> {
    pub configs: &'a BTreeMap<ModelId, ConfiguredModelMetadata>,
    pub only_listed: bool,
    pub provider: Option<&'a str>,
    pub api: &'a str,
    pub protocol: ModelOptionsProtocol,
}

impl ModelResolver<'_> {
    pub fn resolve(
        &self,
        native: &[ListModelEntry],
        catalog: Option<&ModelCatalogView<'_>>,
    ) -> BTreeMap<ModelId, Model> {
        let mut models = BTreeMap::new();
        for entry in native {
            let id = ModelId::new(entry.id.clone());
            if !self.only_listed || self.configs.contains_key(&id) {
                let model = self.resolve_model(id.clone(), Some(entry), catalog);
                models.insert(id, model);
            }
        }
        for id in self.configs.keys() {
            models
                .entry(id.clone())
                .or_insert_with(|| self.resolve_model(id.clone(), None, catalog));
        }
        models
    }

    fn resolve_model(
        &self,
        id: ModelId,
        native: Option<&ListModelEntry>,
        catalog: Option<&ModelCatalogView<'_>>,
    ) -> Model {
        let (mut metadata, options) = catalog
            .and_then(|catalog| {
                catalog.metadata_with_discovery(
                    self.provider,
                    Some(self.api),
                    id.as_str(),
                    native.and_then(|entry| entry.owned_by.as_deref()),
                )
            })
            .unwrap_or_default();
        metadata.options = match native {
            Some(entry) => entry.options.definitions_with_fallback(
                self.protocol,
                entry
                    .supported_reasoning_levels
                    .iter()
                    .map(|level| level.effort.clone()),
                &options,
            ),
            None => options.definitions(self.protocol),
        };
        let fallback = ConfiguredModelMetadata::default();
        self.configs
            .get(&id)
            .unwrap_or(&fallback)
            .resolve(id, Some(metadata))
    }
}
