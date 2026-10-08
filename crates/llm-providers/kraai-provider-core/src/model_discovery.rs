use std::collections::BTreeMap;

use kraai_types::{
    ModelOptionCondition, ModelOptionDefinition, ModelOptionKind, ModelOptionValue,
    ModelRequestPatch,
};
use serde::{Deserialize, Serialize};

use crate::ModelOptionsProtocol;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiscoveredModelOptions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ModelOptionDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_options: Option<Vec<ReasoningOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    experimental: Option<ExperimentalOptions>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ReasoningOption {
    Effort {
        values: Vec<Option<String>>,
    },
    Toggle,
    BudgetTokens {
        min: Option<i64>,
        max: Option<i64>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ExperimentalOptions {
    #[serde(default)]
    modes: BTreeMap<String, CatalogMode>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct CatalogMode {
    #[serde(default)]
    provider: ModelRequestPatch,
}

impl DiscoveredModelOptions {
    pub fn definitions(&self, protocol: ModelOptionsProtocol) -> Vec<ModelOptionDefinition> {
        self.definitions_with_reasoning_levels(protocol, [])
    }

    pub fn definitions_with_reasoning_levels(
        &self,
        protocol: ModelOptionsProtocol,
        levels: impl IntoIterator<Item = String>,
    ) -> Vec<ModelOptionDefinition> {
        let mut options = BTreeMap::new();
        let levels = levels.into_iter().collect::<Vec<_>>();
        if !levels.is_empty() {
            let effort = crate::reasoning_effort_option(protocol, levels);
            options.insert(effort.id.clone(), effort);
        }
        for option in self.reasoning_options.iter().flatten() {
            let definition = match option {
                ReasoningOption::Effort { values } => {
                    let values = values.iter().flatten().cloned().collect::<Vec<_>>();
                    (!values.is_empty()).then(|| crate::reasoning_effort_option(protocol, values))
                }
                ReasoningOption::Toggle => crate::reasoning_toggle_option(protocol),
                ReasoningOption::BudgetTokens { min, max } => {
                    crate::reasoning_budget_option(protocol, *min, *max)
                }
                ReasoningOption::Unknown => None,
            };
            if let Some(definition) = definition {
                options.insert(definition.id.clone(), definition);
            }
        }
        for (id, mode) in self
            .experimental
            .iter()
            .flat_map(|experimental| &experimental.modes)
        {
            let definition = ModelOptionDefinition {
                id: format!("mode:{id}"),
                label: id.clone(),
                description: None,
                required: true,
                active_when: None,
                binding: None,
                kind: ModelOptionKind::Boolean {
                    enabled: mode.provider.clone(),
                    disabled: ModelRequestPatch::default(),
                },
            };
            options.insert(definition.id.clone(), definition);
        }
        for definition in &self.options {
            options.insert(definition.id.clone(), definition.clone());
        }
        if options
            .get("reasoning_enabled")
            .is_some_and(|definition| matches!(definition.kind, ModelOptionKind::Boolean { .. }))
        {
            for id in ["reasoning_effort", "reasoning_budget"] {
                if !self.options.iter().any(|definition| definition.id == id)
                    && let Some(definition) = options.get_mut(id)
                {
                    definition.active_when = Some(ModelOptionCondition {
                        option: "reasoning_enabled".into(),
                        value: ModelOptionValue::Boolean(true),
                    });
                }
            }
        }
        options.into_values().collect()
    }
}
