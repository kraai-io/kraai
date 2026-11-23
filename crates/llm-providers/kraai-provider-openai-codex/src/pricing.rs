use kraai_provider_core::{DynamicConfig, ProviderPricingCatalog, ProviderPricingPolicy};

pub(crate) fn pricing_policy() -> ProviderPricingPolicy {
    ProviderPricingPolicy {
        subscription: true,
        catalog,
    }
}

fn catalog(config: &DynamicConfig) -> ProviderPricingCatalog {
    ProviderPricingCatalog {
        provider: Some(String::from("openai")),
        ..ProviderPricingCatalog::from_config(config)
    }
}

#[cfg(test)]
mod tests;
