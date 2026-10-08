use kraai_types::{
    ModelOptionBinding, ModelOptionChoice, ModelOptionCondition, ModelOptionDefinition,
    ModelOptionKind, ModelOptionValue, ModelRequestPatch,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelOptionsProtocol {
    OpenAiResponses,
    OpenAiChatCompletions,
    AnthropicMessages,
    OpenRouterChatCompletions,
    DeepSeekChatCompletions,
}

pub fn reasoning_effort_option(
    protocol: ModelOptionsProtocol,
    values: impl IntoIterator<Item = String>,
) -> ModelOptionDefinition {
    let path = match protocol {
        ModelOptionsProtocol::OpenAiChatCompletions
        | ModelOptionsProtocol::DeepSeekChatCompletions => "/reasoning_effort",
        ModelOptionsProtocol::AnthropicMessages => "/output_config/effort",
        ModelOptionsProtocol::OpenAiResponses | ModelOptionsProtocol::OpenRouterChatCompletions => {
            "/reasoning/effort"
        }
    };
    option(
        "reasoning_effort",
        "Reasoning effort",
        Some(path),
        ModelOptionKind::Choice {
            choices: values
                .into_iter()
                .map(|value| ModelOptionChoice {
                    label: value.clone(),
                    id: value,
                    patch: ModelRequestPatch::default(),
                })
                .collect(),
        },
    )
}

pub fn reasoning_toggle_option(protocol: ModelOptionsProtocol) -> Option<ModelOptionDefinition> {
    let (path, enabled, disabled) = match protocol {
        ModelOptionsProtocol::AnthropicMessages => (
            None,
            patch(serde_json::json!({"thinking": {"type": "enabled"}})),
            patch(serde_json::json!({"thinking": {"type": "disabled"}})),
        ),
        ModelOptionsProtocol::OpenRouterChatCompletions => (
            Some("/reasoning/enabled"),
            ModelRequestPatch::default(),
            ModelRequestPatch::default(),
        ),
        ModelOptionsProtocol::DeepSeekChatCompletions => (
            None,
            patch(serde_json::json!({"thinking": {"type": "enabled"}})),
            patch(serde_json::json!({"thinking": {"type": "disabled"}})),
        ),
        ModelOptionsProtocol::OpenAiResponses | ModelOptionsProtocol::OpenAiChatCompletions => {
            return None;
        }
    };
    Some(option(
        "reasoning_enabled",
        "Thinking enabled",
        path,
        ModelOptionKind::Boolean { enabled, disabled },
    ))
}

pub fn reasoning_budget_option(
    protocol: ModelOptionsProtocol,
    min: Option<i64>,
    max: Option<i64>,
) -> Option<ModelOptionDefinition> {
    let path = match protocol {
        ModelOptionsProtocol::AnthropicMessages => "/thinking/budget_tokens",
        ModelOptionsProtocol::OpenRouterChatCompletions => "/reasoning/max_tokens",
        ModelOptionsProtocol::OpenAiResponses
        | ModelOptionsProtocol::OpenAiChatCompletions
        | ModelOptionsProtocol::DeepSeekChatCompletions => return None,
    };
    Some(option(
        "reasoning_budget",
        "Thinking token budget",
        Some(path),
        ModelOptionKind::Integer { min, max },
    ))
}

pub fn service_tier_options(choices: Vec<ModelOptionChoice>) -> Vec<ModelOptionDefinition> {
    if choices.is_empty() {
        return Vec::new();
    }
    let enabled = option(
        "processing_mode_enabled",
        "Override processing mode",
        None,
        ModelOptionKind::Boolean {
            enabled: ModelRequestPatch::default(),
            disabled: ModelRequestPatch::default(),
        },
    );
    let mut tier = option(
        "service_tier",
        "Processing mode",
        Some("/service_tier"),
        ModelOptionKind::Choice { choices },
    );
    tier.active_when = Some(ModelOptionCondition {
        option: enabled.id.clone(),
        value: ModelOptionValue::Boolean(true),
    });
    vec![enabled, tier]
}

fn option(
    id: &str,
    label: &str,
    path: Option<&str>,
    kind: ModelOptionKind,
) -> ModelOptionDefinition {
    ModelOptionDefinition {
        id: id.into(),
        label: label.into(),
        description: None,
        required: true,
        active_when: None,
        binding: path.map(|path| ModelOptionBinding::Body { path: path.into() }),
        kind,
    }
}

fn patch(value: serde_json::Value) -> ModelRequestPatch {
    ModelRequestPatch {
        body: value
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        headers: Default::default(),
    }
}
