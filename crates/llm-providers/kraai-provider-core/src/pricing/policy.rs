use crate::{DynamicConfig, DynamicValue};

#[derive(Clone, Copy)]
pub struct ProviderPricingPolicy {
    pub subscription: bool,
    pub catalog: fn(&DynamicConfig) -> ProviderPricingCatalog,
}

impl Default for ProviderPricingPolicy {
    fn default() -> Self {
        Self {
            subscription: false,
            catalog: ProviderPricingCatalog::from_config,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderPricingCatalog {
    pub api: Option<String>,
    pub provider: Option<String>,
}

impl ProviderPricingCatalog {
    pub fn from_config(config: &DynamicConfig) -> Self {
        Self {
            api: config
                .get("base_url")
                .and_then(DynamicValue::as_str)
                .map(|value| value.trim().to_string()),
            provider: None,
        }
    }
}
