use std::collections::{BTreeMap, BTreeSet};

use color_eyre::eyre::{Result, ensure};
use kraai_provider_core::{ConfiguredModelMetadata, Model, ModelOptionsProtocol};
use kraai_types::{ModelId, ModelOptionChoice, ModelRequestPatch};

use crate::wire::{ListModelEntry, ModelVisibility};

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;

#[derive(Default)]
pub(crate) struct DiscoveredModels {
    entries: BTreeMap<String, ListModelEntry>,
}

impl DiscoveredModels {
    pub(crate) fn new(models: Vec<ListModelEntry>) -> Result<Self> {
        let mut entries = BTreeMap::new();
        for model in models {
            ensure!(
                !model.slug.trim().is_empty(),
                "Codex discovery returned an empty model slug"
            );
            for (kind, values) in [
                (
                    "reasoning effort",
                    model
                        .supported_reasoning_levels
                        .iter()
                        .map(|level| level.effort.as_str())
                        .collect::<Vec<_>>(),
                ),
                (
                    "service tier",
                    model
                        .service_tiers
                        .iter()
                        .map(|tier| tier.id.as_str())
                        .collect::<Vec<_>>(),
                ),
            ] {
                let mut unique = BTreeSet::new();
                ensure!(
                    values
                        .into_iter()
                        .all(|value| !value.trim().is_empty() && unique.insert(value)),
                    "Codex model '{}' returned an empty or duplicate {kind}",
                    model.slug
                );
            }
            let slug = model.slug.clone();
            ensure!(
                entries.insert(slug.clone(), model).is_none(),
                "Codex discovery returned duplicate model '{slug}'"
            );
        }
        Ok(Self { entries })
    }

    pub(crate) fn list(&self, configs: &BTreeMap<ModelId, ConfiguredModelMetadata>) -> Vec<Model> {
        let mut models = self
            .entries
            .values()
            .filter(|entry| entry.visibility == ModelVisibility::List)
            .map(|entry| Self::listed_model(entry, configs))
            .collect::<Vec<_>>();
        models.extend(
            configs
                .iter()
                .filter(|(id, _)| !self.entries.contains_key(id.as_str()))
                .map(|(id, config)| config.resolve(id.clone(), None)),
        );
        models.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        models
    }

    pub(crate) fn get(
        &self,
        id: &ModelId,
        configs: &BTreeMap<ModelId, ConfiguredModelMetadata>,
    ) -> Option<Model> {
        match self.entries.get(id.as_str()) {
            Some(entry) => (entry.visibility == ModelVisibility::List)
                .then(|| Self::listed_model(entry, configs)),
            None => configs
                .get(id)
                .map(|config| config.resolve(id.clone(), None)),
        }
    }

    fn listed_model(
        entry: &ListModelEntry,
        configs: &BTreeMap<ModelId, ConfiguredModelMetadata>,
    ) -> Model {
        let id = ModelId::new(entry.slug.clone());
        let config = configs.get(&id).cloned().unwrap_or_default();
        let mut options = Vec::new();
        if !entry.supported_reasoning_levels.is_empty() {
            options.push(kraai_provider_core::reasoning_effort_option(
                ModelOptionsProtocol::OpenAiResponses,
                entry
                    .supported_reasoning_levels
                    .iter()
                    .map(|level| level.effort.clone()),
            ));
        }
        if !entry.service_tiers.is_empty() {
            options.extend(kraai_provider_core::service_tier_options(
                entry
                    .service_tiers
                    .iter()
                    .map(|tier| ModelOptionChoice {
                        id: tier.id.clone(),
                        label: tier.name.clone(),
                        patch: ModelRequestPatch::default(),
                    })
                    .collect(),
            ));
        }
        Model {
            id,
            name: config.name.clone().unwrap_or_else(|| {
                if entry.display_name.trim().is_empty() {
                    entry.slug.clone()
                } else {
                    entry.display_name.clone()
                }
            }),
            max_context: config.max_context.or(entry.context_window),
            supports_images: config.supports_images.unwrap_or_else(|| {
                entry
                    .input_modalities
                    .iter()
                    .any(|modality| modality == "image")
            }),
            options: config.merge_options(options),
        }
    }
}
