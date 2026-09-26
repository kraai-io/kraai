use std::collections::{BTreeMap, BTreeSet};

use color_eyre::eyre::{Result, ensure, eyre};
use kraai_provider_core::{ConfiguredModelMetadata, Model};
use kraai_types::ModelId;

use crate::wire::{ListModelEntry, ModelVisibility, ResponsesReasoning};

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ResolvedRequestModel {
    pub(crate) api_model: String,
    pub(crate) reasoning: Option<ResponsesReasoning>,
}

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
            let mut efforts = BTreeSet::new();
            for level in &model.supported_reasoning_levels {
                ensure!(
                    !level.effort.trim().is_empty() && efforts.insert(level.effort.as_str()),
                    "Codex model '{}' returned an empty or duplicate reasoning level",
                    model.slug
                );
            }
            if let Some(default) = &model.default_reasoning_level {
                ensure!(
                    efforts.contains(default.as_str()),
                    "Codex model '{}' returned an unsupported default reasoning level '{default}'",
                    model.slug
                );
            }
            let slug = model.slug.clone();
            ensure!(
                entries.insert(slug.clone(), model).is_none(),
                "Codex discovery returned duplicate model '{slug}'"
            );
        }
        let mut ids = entries.keys().cloned().collect::<BTreeSet<_>>();
        for model in entries.values() {
            for level in &model.supported_reasoning_levels {
                let id = format!("{}-{}", model.slug, level.effort);
                ensure!(
                    ids.insert(id.clone()),
                    "Codex discovery returned an ambiguous model variant '{id}'"
                );
            }
        }
        Ok(Self { entries })
    }

    pub(crate) fn list(&self, configs: &BTreeMap<ModelId, ConfiguredModelMetadata>) -> Vec<Model> {
        let mut models = Vec::new();
        for entry in self
            .entries
            .values()
            .filter(|entry| entry.visibility == ModelVisibility::List)
        {
            if entry.supported_reasoning_levels.is_empty() {
                models.push(Self::listed_model(entry, None, configs));
            } else {
                for level in &entry.supported_reasoning_levels {
                    models.push(Self::listed_model(entry, Some(&level.effort), configs));
                }
            }
        }
        models.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        models
    }

    pub(crate) fn get(
        &self,
        id: &ModelId,
        configs: &BTreeMap<ModelId, ConfiguredModelMetadata>,
    ) -> Option<Model> {
        let raw = id.as_str();
        if let Some(entry) = self.entries.get(raw) {
            return (entry.visibility == ModelVisibility::List
                && entry.supported_reasoning_levels.is_empty())
            .then(|| Self::listed_model(entry, None, configs));
        }
        raw.rmatch_indices('-').find_map(|(index, _)| {
            let entry = self.entries.get(raw.get(..index)?)?;
            let effort = raw.get(index.saturating_add(1)..)?;
            (entry.visibility == ModelVisibility::List
                && entry
                    .supported_reasoning_levels
                    .iter()
                    .any(|level| level.effort == effort))
            .then(|| Self::listed_model(entry, Some(effort), configs))
        })
    }

    fn listed_model(
        entry: &ListModelEntry,
        effort: Option<&str>,
        configs: &BTreeMap<ModelId, ConfiguredModelMetadata>,
    ) -> Model {
        let base_id = ModelId::new(entry.slug.clone());
        let id = effort.map_or_else(
            || base_id.clone(),
            |effort| ModelId::new(format!("{}-{effort}", entry.slug)),
        );
        let variant_config = configs.get(&id);
        let base_config = configs.get(&base_id);
        let name = base_config
            .and_then(|config| config.name.as_deref())
            .unwrap_or_else(|| {
                if entry.display_name.trim().is_empty() {
                    &entry.slug
                } else {
                    &entry.display_name
                }
            });
        Model {
            supports_images: image_support(entry, &id, configs),
            id,
            name: variant_config
                .and_then(|config| config.name.clone())
                .unwrap_or_else(|| {
                    effort.map_or_else(|| name.to_string(), |effort| format!("{name} {effort}"))
                }),
            max_context: variant_config
                .and_then(|config| config.max_context)
                .or(base_config.and_then(|config| config.max_context))
                .or(entry.context_window),
        }
    }

    pub(crate) fn resolve(&self, model_id: &ModelId) -> Result<ResolvedRequestModel> {
        let raw = model_id.as_str();
        let (model, effort) = if let Some(model) = self.entries.get(raw) {
            (model, model.default_reasoning_level.as_deref())
        } else {
            let (model, suffix) = raw
                .rmatch_indices('-')
                .find_map(|(index, _)| {
                    let prefix = raw.get(..index)?;
                    let suffix = raw.get(index.saturating_add(1)..)?;
                    self.entries.get(prefix).map(|model| (model, suffix))
                })
                .ok_or_else(|| {
                    eyre!("OpenAI Codex model '{raw}' was not returned by model discovery")
                })?;
            ensure!(
                model
                    .supported_reasoning_levels
                    .iter()
                    .any(|level| level.effort == suffix),
                "OpenAI Codex model '{raw}' uses unsupported reasoning effort '{suffix}' for base model '{}'",
                model.slug
            );
            (model, Some(suffix))
        };
        Ok(ResolvedRequestModel {
            api_model: model.slug.clone(),
            reasoning: effort.map(|effort| ResponsesReasoning {
                effort: effort.to_string(),
                context: "all_turns",
            }),
        })
    }

    pub(crate) fn supports_images(
        &self,
        id: &ModelId,
        configs: &BTreeMap<ModelId, ConfiguredModelMetadata>,
    ) -> Result<bool> {
        let resolved = self.resolve(id)?;
        let entry = self
            .entries
            .get(&resolved.api_model)
            .ok_or_else(|| eyre!("Resolved model is missing from discovery"))?;
        Ok(image_support(entry, id, configs))
    }
}

fn image_support(
    entry: &ListModelEntry,
    id: &ModelId,
    configs: &BTreeMap<ModelId, ConfiguredModelMetadata>,
) -> bool {
    configs
        .get(id)
        .and_then(|config| config.supports_images)
        .or_else(|| {
            configs
                .get(&ModelId::new(entry.slug.clone()))
                .and_then(|config| config.supports_images)
        })
        .unwrap_or_else(|| {
            entry
                .input_modalities
                .iter()
                .any(|modality| modality == "image")
        })
}
