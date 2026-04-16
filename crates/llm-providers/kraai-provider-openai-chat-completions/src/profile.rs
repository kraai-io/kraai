use color_eyre::eyre::{Result, eyre};
use kraai_provider_core::{
    DynamicConfig, DynamicValue, FieldDefinition, FieldValueKind, ProviderDefinition,
    ValidationError,
};

pub trait ChatCompletionsProfile: Send + Sync + 'static {
    const TYPE_ID: &'static str;
    const DISPLAY_NAME: &'static str;
    const DESCRIPTION: &'static str;
    const DEFAULT_PROVIDER_ID_PREFIX: &'static str;

    fn base_url(config: &DynamicConfig) -> Result<String>;

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
            model_fields: vec![
                FieldDefinition {
                    key: String::from("name"),
                    label: String::from("Display Name"),
                    value_kind: FieldValueKind::String,
                    required: false,
                    secret: false,
                    help_text: Some(String::from("Optional UI name for the model")),
                    default_value: None,
                },
                FieldDefinition {
                    key: String::from("max_context"),
                    label: String::from("Max Context"),
                    value_kind: FieldValueKind::Integer,
                    required: false,
                    secret: false,
                    help_text: Some(String::from("Optional context limit in tokens")),
                    default_value: None,
                },
            ],
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
        let mut errors = Vec::new();
        if let Some(value) = config.get("name")
            && value.as_str().is_none()
        {
            errors.push(ValidationError {
                field: String::from("name"),
                message: String::from("Display Name must be a string"),
            });
        }
        if let Some(value) = config.get("max_context") {
            match value.as_integer() {
                Some(number) if number > 0 => {}
                Some(_) => errors.push(ValidationError {
                    field: String::from("max_context"),
                    message: String::from("Max Context must be greater than zero"),
                }),
                None => errors.push(ValidationError {
                    field: String::from("max_context"),
                    message: String::from("Max Context must be an integer"),
                }),
            }
        }
        errors
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
