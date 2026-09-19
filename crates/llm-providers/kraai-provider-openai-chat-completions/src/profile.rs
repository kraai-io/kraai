use color_eyre::eyre::{Result, eyre};
use kraai_provider_core::{
    ConfiguredModelMetadata, DynamicConfig, DynamicValue, FieldDefinition, FieldValueKind,
    ProviderDefinition, ProviderPricingCatalog, ProviderPricingPolicy, ValidationError,
};

pub trait ChatCompletionsProfile: Send + Sync + 'static {
    const TYPE_ID: &'static str;
    const DISPLAY_NAME: &'static str;
    const DESCRIPTION: &'static str;
    const DEFAULT_PROVIDER_ID_PREFIX: &'static str;

    fn base_url(config: &DynamicConfig) -> Result<String>;

    fn pricing_policy() -> ProviderPricingPolicy {
        ProviderPricingPolicy {
            subscription: false,
            catalog: Self::pricing_catalog,
        }
    }

    fn pricing_catalog(config: &DynamicConfig) -> ProviderPricingCatalog {
        let mut catalog = ProviderPricingCatalog::from_config(config);
        catalog.api = catalog.api.or_else(Self::default_base_url);
        catalog.provider = (catalog.api.as_deref().map(|url| url.trim_end_matches('/'))
            == Some("https://api.openai.com/v1"))
        .then(|| String::from("openai"));
        catalog
    }

    fn definition() -> ProviderDefinition {
        ProviderDefinition {
            type_id: String::new(),
            display_name: Self::DISPLAY_NAME.to_string(),
            protocol_family: String::from("openai-chat-completions"),
            description: Self::DESCRIPTION.to_string(),
            provider_fields: vec![
                FieldDefinition {
                    key: String::from("base_url"),
                    label: String::from("Base URL"),
                    value_kind: FieldValueKind::Url,
                    required: Self::TYPE_ID == "openai-chat-completions",
                    secret: false,
                    help_text: Some(String::from(
                        "API base URL including version path, for example https://api.openai.com/v1",
                    )),
                    default_value: Self::default_base_url().map(DynamicValue::String),
                },
                FieldDefinition {
                    key: String::from("api_key"),
                    label: String::from("Inline API Key"),
                    value_kind: FieldValueKind::SecretString,
                    required: false,
                    secret: true,
                    help_text: Some(String::from("Inline bearer token for API-key auth")),
                    default_value: None,
                },
                FieldDefinition {
                    key: String::from("env_var_api_key"),
                    label: String::from("Env Var"),
                    value_kind: FieldValueKind::String,
                    required: false,
                    secret: false,
                    help_text: Some(String::from("Environment variable that stores the API key")),
                    default_value: Some(DynamicValue::from("OPENAI_API_KEY")),
                },
                FieldDefinition {
                    key: String::from("only_listed_models"),
                    label: String::from("Only Listed Models"),
                    value_kind: FieldValueKind::Boolean,
                    required: false,
                    secret: false,
                    help_text: Some(String::from(
                        "When enabled, only models explicitly configured in providers.toml are shown",
                    )),
                    default_value: Some(DynamicValue::Bool(true)),
                },
            ],
            model_fields: ConfiguredModelMetadata::fields(),
            supports_model_discovery: true,
            default_provider_id_prefix: Self::DEFAULT_PROVIDER_ID_PREFIX.to_string(),
        }
    }

    fn validate_provider_config(config: &DynamicConfig) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        if Self::TYPE_ID == "openai-chat-completions"
            && config
                .get("base_url")
                .and_then(DynamicValue::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
        {
            errors.push(ValidationError {
                field: String::from("base_url"),
                message: String::from("Base URL is required"),
            });
        }

        let inline_key = config
            .get("api_key")
            .and_then(DynamicValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let env_var = config
            .get("env_var_api_key")
            .and_then(DynamicValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());

        if inline_key.is_none() && env_var.is_none() {
            errors.push(ValidationError {
                field: String::from("credentials"),
                message: String::from("Provide either an API key or an environment variable name"),
            });
        }

        if let Some(value) = config.get("only_listed_models")
            && value.as_bool().is_none()
        {
            errors.push(ValidationError {
                field: String::from("only_listed_models"),
                message: String::from("Only Listed Models must be a boolean"),
            });
        }

        errors
    }

    fn validate_model_config(config: &DynamicConfig) -> Vec<ValidationError> {
        ConfiguredModelMetadata::validate(config)
    }

    fn default_base_url() -> Option<String> {
        None
    }
}

pub struct GenericChatCompletionsProfile;

impl ChatCompletionsProfile for GenericChatCompletionsProfile {
    const TYPE_ID: &'static str = "openai-chat-completions";
    const DISPLAY_NAME: &'static str = "OpenAI-compatible Chat Completions";
    const DESCRIPTION: &'static str = "Generic OpenAI-compatible chat-completions provider";
    const DEFAULT_PROVIDER_ID_PREFIX: &'static str = "openai-chat-completions";

    fn base_url(config: &DynamicConfig) -> Result<String> {
        config
            .get("base_url")
            .and_then(DynamicValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .ok_or_else(|| eyre!("Base URL is required"))
    }
}

pub struct OpenAiChatCompletionsProfile;

impl ChatCompletionsProfile for OpenAiChatCompletionsProfile {
    const TYPE_ID: &'static str = "openai";
    const DISPLAY_NAME: &'static str = "OpenAI Chat Completions";
    const DESCRIPTION: &'static str = "OpenAI chat-completions provider with OpenAI defaults";
    const DEFAULT_PROVIDER_ID_PREFIX: &'static str = "openai";

    fn base_url(config: &DynamicConfig) -> Result<String> {
        Ok(config
            .get("base_url")
            .and_then(DynamicValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| String::from("https://api.openai.com/v1")))
    }

    fn default_base_url() -> Option<String> {
        Some(String::from("https://api.openai.com/v1"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pricing_catalog_preserves_endpoint_defaults_and_overrides() {
        for (policy, default_api) in [
            (GenericChatCompletionsProfile::pricing_policy(), None),
            (
                OpenAiChatCompletionsProfile::pricing_policy(),
                Some("https://api.openai.com/v1"),
            ),
        ] {
            assert!(!policy.subscription);
            let default_catalog = (policy.catalog)(&DynamicConfig::new());
            assert_eq!(default_catalog.api.as_deref(), default_api);
            assert_eq!(
                default_catalog.provider.as_deref(),
                default_api.map(|_| "openai")
            );
            for (base_url, expected_provider) in [
                ("https://api.openai.com/v1", Some("openai")),
                ("  https://api.openai.com/v1///  ", Some("openai")),
                ("https://proxy.test/v1", None),
                ("  ", None),
            ] {
                let catalog = (policy.catalog)(&DynamicConfig::from([(
                    "base_url".into(),
                    DynamicValue::from(base_url),
                )]));
                assert_eq!(catalog.api.as_deref(), Some(base_url.trim()));
                assert_eq!(catalog.provider.as_deref(), expected_provider);
            }
            let invalid_catalog = (policy.catalog)(&DynamicConfig::from([(
                "base_url".into(),
                DynamicValue::Bool(false),
            )]));
            assert_eq!(invalid_catalog, default_catalog);
        }
    }
}
