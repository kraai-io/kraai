use std::collections::{BTreeMap, BTreeSet};

use color_eyre::eyre::{Result, ensure, eyre};
use kraai_provider_core::Model;
use kraai_types::ModelId;

use crate::wire::{ListModelEntry, ResponsesReasoning};

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;

#[derive(Clone)]
pub(crate) struct ModelMetadata {
    pub(crate) name: Option<String>,
    pub(crate) max_context: Option<usize>,
}

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

    pub(crate) fn list(&self, configs: &BTreeMap<ModelId, ModelMetadata>) -> Vec<Model> {
        let mut models = BTreeMap::new();
        for entry in self
            .entries
            .values()
            .filter(|entry| entry.visibility == "list")
        {
            if entry.supported_reasoning_levels.is_empty() {
                let model = Self::listed_model(entry, None, configs);
                models.insert(model.id.clone(), model);
            } else {
                for level in &entry.supported_reasoning_levels {
                    let model = Self::listed_model(entry, Some(&level.effort), configs);
                    models.insert(model.id.clone(), model);
                }
            }
        }
        models.into_values().collect()
    }

    fn listed_model(
        entry: &ListModelEntry,
        effort: Option<&str>,
        configs: &BTreeMap<ModelId, ModelMetadata>,
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
            let (model, suffix) = self
                .entries
                .values()
                .filter_map(|model| {
                    raw.strip_prefix(&model.slug)
                        .and_then(|suffix| suffix.strip_prefix('-'))
                        .map(|suffix| (model, suffix))
                })
                .max_by_key(|(model, _)| model.slug.len())
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
                context: "current_turn",
            }),
        })
    }
}
