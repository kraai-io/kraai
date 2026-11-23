use std::collections::BTreeMap;
use std::sync::Arc;

use color_eyre::Result;
use kraai_types::ProviderId;

use crate::ProviderPricingPolicy;
use crate::config::DynamicConfig;
use crate::definition::{ProviderDefinition, ValidationError};
use crate::error::ProviderError;
use crate::provider::Provider;

#[derive(Default, Clone)]
pub struct ProviderRegistry {
    factories: Arc<BTreeMap<String, Arc<FactoryEntry>>>,
}

struct FactoryEntry {
    definition: ProviderDefinition,
    pricing_policy: ProviderPricingPolicy,
    create: Box<ProviderFactoryFn>,
    validate_provider_config: Box<ValidateConfigFn>,
    validate_model_config: Box<ValidateConfigFn>,
}

type ProviderFactoryFn =
    dyn Fn(ProviderId, DynamicConfig) -> Result<Box<dyn Provider>, ProviderError> + Send + Sync;
type ValidateConfigFn = dyn Fn(&DynamicConfig) -> Vec<ValidationError> + Send + Sync;

pub trait ProviderFactory {
    const TYPE_ID: &'static str;

    fn definition() -> ProviderDefinition;

    fn pricing_policy() -> ProviderPricingPolicy;

    fn create(id: ProviderId, config: DynamicConfig) -> Result<Box<dyn Provider>>;

    fn validate_provider_config(_config: &DynamicConfig) -> Vec<ValidationError> {
        Vec::new()
    }

    fn validate_model_config(_config: &DynamicConfig) -> Vec<ValidationError> {
        Vec::new()
    }
}

impl ProviderRegistry {
    pub fn register_factory<F: ProviderFactory + 'static>(&mut self) -> Result<(), ProviderError> {
        self.register_dynamic_factory(
            F::TYPE_ID,
            F::definition(),
            F::pricing_policy(),
            |id, config| {
                F::create(id, config)
                    .map_err(|error| ProviderError::ConfigParseError(error.to_string()))
            },
            F::validate_provider_config,
            F::validate_model_config,
        )
    }

    pub fn register_dynamic_factory<C, VP, VM>(
        &mut self,
        type_id: impl Into<String>,
        mut definition: ProviderDefinition,
        pricing_policy: ProviderPricingPolicy,
        create: C,
        validate_provider_config: VP,
        validate_model_config: VM,
    ) -> Result<(), ProviderError>
    where
        C: Fn(ProviderId, DynamicConfig) -> Result<Box<dyn Provider>, ProviderError>
            + Send
            + Sync
            + 'static,
        VP: Fn(&DynamicConfig) -> Vec<ValidationError> + Send + Sync + 'static,
        VM: Fn(&DynamicConfig) -> Vec<ValidationError> + Send + Sync + 'static,
    {
        let key = type_id.into();
        if self.factories.contains_key(&key) {
            return Err(ProviderError::FactoryAlreadyRegistered(key));
        }

        definition.type_id = key.clone();
        if !pricing_policy.subscription {
            definition
                .provider_fields
                .extend(crate::pricing::pricing_fields(false));
            definition
                .model_fields
                .extend(crate::pricing::pricing_fields(true));
        }

        let entry = FactoryEntry {
            definition,
            pricing_policy,
            create: Box::new(create),
            validate_provider_config: Box::new(validate_provider_config),
            validate_model_config: Box::new(validate_model_config),
        };

        Arc::make_mut(&mut self.factories).insert(key, Arc::new(entry));
        Ok(())
    }

    pub fn has_factory(&self, provider_type: &str) -> bool {
        self.factories.contains_key(provider_type)
    }

    pub fn list_definitions(&self) -> Vec<ProviderDefinition> {
        self.factories
            .values()
            .map(|entry| entry.definition.clone())
            .collect()
    }

    pub fn get_definition(&self, type_id: &str) -> Option<ProviderDefinition> {
        self.factories
            .get(type_id)
            .map(|entry| entry.definition.clone())
    }

    pub fn pricing_policy(&self, type_id: &str) -> Option<ProviderPricingPolicy> {
        self.factories
            .get(type_id)
            .map(|entry| entry.pricing_policy)
    }

    pub fn validate_provider_config(
        &self,
        type_id: &str,
        config: &DynamicConfig,
    ) -> Result<Vec<ValidationError>, ProviderError> {
        let entry = self
            .factories
            .get(type_id)
            .ok_or_else(|| ProviderError::UnknownProviderType(type_id.to_string()))?;
        let mut errors = (entry.validate_provider_config)(config);
        if config
            .get("pricing_provider")
            .is_some_and(|value| value.as_str().is_none())
        {
            errors.push(ValidationError {
                field: String::from("pricing_provider"),
                message: String::from("Pricing provider must be a models.dev provider ID"),
            });
        }
        Ok(errors)
    }

    pub fn validate_model_config(
        &self,
        type_id: &str,
        config: &DynamicConfig,
    ) -> Result<Vec<ValidationError>, ProviderError> {
        let entry = self
            .factories
            .get(type_id)
            .ok_or_else(|| ProviderError::UnknownProviderType(type_id.to_string()))?;
        let mut errors = (entry.validate_model_config)(config);
        errors.extend(crate::pricing::validate_pricing_config(config));
        Ok(errors)
    }

    pub(crate) fn create_provider(
        &self,
        type_id: &str,
        id: ProviderId,
        config: DynamicConfig,
    ) -> Result<Box<dyn Provider>, ProviderError> {
        let entry = self
            .factories
            .get(type_id)
            .ok_or_else(|| ProviderError::UnknownProviderType(type_id.to_string()))?;
        (entry.create)(id, config)
    }
}

#[cfg(test)]
mod tests;
