use kraai_provider_core::{DynamicConfig, DynamicValue};
use reqwest::RequestBuilder;

#[derive(Clone)]
pub struct ApiKeyAuth {
    api_key: Option<String>,
}

impl ApiKeyAuth {
    pub fn resolve(config: &DynamicConfig) -> Self {
        let inline_key = config
            .get("api_key")
            .and_then(DynamicValue::as_str)
            .map(ToString::to_string)
            .filter(|value| !value.trim().is_empty());
        let api_key = inline_key.or_else(|| {
            config
                .get("env_var_api_key")
                .and_then(DynamicValue::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .and_then(|name| std::env::var(name).ok())
                .filter(|value| !value.trim().is_empty())
        });
        Self { api_key }
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
