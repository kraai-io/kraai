use color_eyre::eyre::{Result, eyre};
use kraai_provider_core::{DynamicConfig, DynamicValue};
use reqwest::RequestBuilder;

#[derive(Clone)]
pub struct ApiKeyAuth {
    api_key: Option<String>,
}

impl ApiKeyAuth {
    pub fn resolve(config: &DynamicConfig) -> Result<Self> {
        let setting = |key: &str| {
            config
                .get(key)
                .and_then(DynamicValue::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let api_key = match (setting("api_key"), setting("env_var_api_key")) {
            (Some(key), _) => Some(key.to_owned()),
            (None, Some(name)) => Some(
                std::env::var(name)
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        eyre!(
                            "API key environment variable {name} is not set. Set it, or clear the Env Var field if the server needs no auth"
                        )
                    })?,
            ),
            (None, None) => None,
        };
        Ok(Self { api_key })
    }

    pub fn apply(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.api_key {
            Some(api_key) => request.bearer_auth(api_key),
            None => request,
        }
    }
}

#[cfg(test)]
mod tests;
