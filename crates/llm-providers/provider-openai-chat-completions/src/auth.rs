use color_eyre::eyre::{Result, eyre};
use provider_core::{DynamicConfig, DynamicValue};
use reqwest::RequestBuilder;

#[derive(Clone)]
pub struct ApiKeyAuth {
    api_key: String,
}

impl ApiKeyAuth {
    pub fn resolve(config: &DynamicConfig) -> Result<Self> {
        let inline_key = config
            .get("api_key")
            .and_then(DynamicValue::as_str)
            .map(ToString::to_string)
            .filter(|value| !value.trim().is_empty());
        let env_var_name = config
            .get("env_var_api_key")
            .and_then(DynamicValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);

        let api_key = inline_key
            .or_else(|| {
                env_var_name
                    .as_ref()
                    .and_then(|name| std::env::var(name).ok())
            })
            .ok_or_else(|| eyre!("API key not found. Provide 'api_key' or 'env_var_api_key'"))?;

        Ok(Self { api_key })
    }

    pub fn apply(&self, request: RequestBuilder) -> RequestBuilder {
        request.bearer_auth(&self.api_key)
    }
}
