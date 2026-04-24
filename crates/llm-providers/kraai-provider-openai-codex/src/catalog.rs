#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatalogVisibility {
    List,
    Hide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CatalogReasoningEffort {
    pub(crate) effort: &'static str,
    pub(crate) description: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CatalogModel {
    pub(crate) slug: &'static str,
    pub(crate) display_name: &'static str,
    pub(crate) visibility: CatalogVisibility,
    pub(crate) max_context: Option<usize>,
    pub(crate) default_reasoning_effort: &'static str,
    pub(crate) supported_reasoning_efforts: &'static [CatalogReasoningEffort],
}

const GENERAL_REASONING_EFFORTS: [CatalogReasoningEffort; 4] = [
    CatalogReasoningEffort {
        effort: "low",
        description: "Fast responses with lighter reasoning",
    },
    CatalogReasoningEffort {
        effort: "medium",
        description: "Balances speed and reasoning depth for everyday tasks",
    },
    CatalogReasoningEffort {
        effort: "high",
        description: "Greater reasoning depth for complex problems",
    },
    CatalogReasoningEffort {
        effort: "xhigh",
        description: "Extra high reasoning depth for complex problems",
    },
];

const GPT_5_2_REASONING_EFFORTS: [CatalogReasoningEffort; 4] = [
    CatalogReasoningEffort {
        effort: "low",
        description: "Balances speed with some reasoning; useful for straightforward queries and short explanations",
    },
    CatalogReasoningEffort {
        effort: "medium",
        description: "Provides a solid balance of reasoning depth and latency for general-purpose tasks",
    },
    CatalogReasoningEffort {
        effort: "high",
        description: "Maximizes reasoning depth for complex or ambiguous problems",
    },
    CatalogReasoningEffort {
        effort: "xhigh",
        description: "Extra high reasoning for complex problems",
    },
];

const CATALOG_MODELS: [CatalogModel; 6] = [
    CatalogModel {
        slug: "gpt-5.5",
        display_name: "gpt-5.5",
        visibility: CatalogVisibility::List,
        max_context: Some(272_000),
        default_reasoning_effort: "medium",
        supported_reasoning_efforts: &GENERAL_REASONING_EFFORTS,
    },
    CatalogModel {
        slug: "gpt-5.4",
        display_name: "gpt-5.4",
        visibility: CatalogVisibility::List,
        max_context: Some(272_000),
        default_reasoning_effort: "medium",
        supported_reasoning_efforts: &GENERAL_REASONING_EFFORTS,
    },
    CatalogModel {
        slug: "gpt-5.4-mini",
        display_name: "gpt-5.4-Mini",
        visibility: CatalogVisibility::List,
        max_context: Some(272_000),
        default_reasoning_effort: "medium",
        supported_reasoning_efforts: &GENERAL_REASONING_EFFORTS,
    },
    CatalogModel {
        slug: "gpt-5.3-codex",
        display_name: "gpt-5.3-codex",
        visibility: CatalogVisibility::List,
        max_context: Some(272_000),
        default_reasoning_effort: "medium",
        supported_reasoning_efforts: &GENERAL_REASONING_EFFORTS,
    },
    CatalogModel {
        slug: "gpt-5.2",
        display_name: "gpt-5.2",
        visibility: CatalogVisibility::List,
        max_context: Some(272_000),
        default_reasoning_effort: "medium",
        supported_reasoning_efforts: &GPT_5_2_REASONING_EFFORTS,
    },
    CatalogModel {
        slug: "codex-auto-review",
        display_name: "Codex Auto Review",
        visibility: CatalogVisibility::Hide,
        max_context: Some(272_000),
        default_reasoning_effort: "medium",
        supported_reasoning_efforts: &GENERAL_REASONING_EFFORTS,
    },
];

pub(crate) fn all_catalog_models() -> &'static [CatalogModel] {
    &CATALOG_MODELS
}

pub(crate) fn visible_catalog_models() -> impl Iterator<Item = &'static CatalogModel> {
    CATALOG_MODELS
        .iter()
        .filter(|model| model.visibility == CatalogVisibility::List)
}

pub(crate) fn title_case_effort(effort: &str) -> String {
    let mut chars = effort.chars();
    match chars.next() {
        Some(first) => {
            let mut output = first.to_ascii_uppercase().to_string();
            output.push_str(chars.as_str());
            output
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_catalog_models_hide_deprecated_entries() {
        let visible = visible_catalog_models()
            .map(|model| model.slug)
            .collect::<Vec<_>>();

        assert!(visible.contains(&"gpt-5.5"));
        assert!(visible.contains(&"gpt-5.4-mini"));
        assert!(visible.contains(&"gpt-5.3-codex"));
        assert!(!visible.contains(&"codex-auto-review"));
    }

    #[test]
    fn lookup_finds_hidden_catalog_model() {
        let model = all_catalog_models()
            .iter()
            .find(|model| model.slug == "codex-auto-review")
            .expect("hidden model");
        assert_eq!(model.display_name, "Codex Auto Review");
        assert_eq!(model.default_reasoning_effort, "medium");
    }
}
