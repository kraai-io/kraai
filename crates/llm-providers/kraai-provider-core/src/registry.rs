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
    create: Arc<ProviderFactoryFn>,
    validate_provider_config: Arc<ValidateConfigFn>,
    validate_model_config: Arc<ValidateConfigFn>,
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
        let mut definition = F::definition();
        definition.type_id = F::TYPE_ID.to_string();

        self.register_dynamic_factory(
            F::TYPE_ID,
            definition,
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
            create: Arc::new(create),
            validate_provider_config: Arc::new(validate_provider_config),
            validate_model_config: Arc::new(validate_model_config),
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
#[expect(
    clippy::panic_in_result_fn,
    reason = "fallible provider setup is combined with direct assertions"
)]
mod tests {
    use super::*;
    use color_eyre::eyre::eyre;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::test_support::{MockFactory, MockProvider, simple_provider_definition};

    #[test]
    fn pricing_fields_are_only_exposed_for_metered_providers() -> Result<()> {
        let mut registry = ProviderRegistry::default();
        for (type_id, subscription) in [("flat-rate", true), ("metered", false)] {
            registry.register_dynamic_factory(
                type_id,
                simple_provider_definition("Provider", "Provider", false, type_id),
                ProviderPricingPolicy {
                    subscription,
                    ..ProviderPricingPolicy::default()
                },
                |id, _config| Ok(Box::new(MockProvider::new(id.as_str()))),
                |_| Vec::new(),
                |_| Vec::new(),
            )?;
            let definition = registry
                .get_definition(type_id)
                .ok_or_else(|| eyre!("missing definition"))?;
            assert_eq!(
                definition
                    .provider_fields
                    .iter()
                    .any(|field| field.key == "pricing_provider"),
                !subscription
            );
            assert_eq!(
                definition
                    .model_fields
                    .iter()
                    .any(|field| field.key.starts_with("price_")),
                !subscription
            );
        }
        Ok(())
    }

    #[test]
    fn test_registry_registration() -> Result<()> {
        let mut registry = ProviderRegistry::default();
        registry.register_factory::<MockFactory>()?;
        assert!(registry.has_factory("mock"));
        assert_eq!(
            registry
                .get_definition("mock")
                .ok_or_else(|| eyre!("mock factory definition missing"))?
                .display_name,
            "Mock".to_string()
        );
        Ok(())
    }

    #[test]
    fn cloned_registries_isolate_registration_and_rejected_duplicates() -> Result<()> {
        let mut registry = ProviderRegistry::default();
        registry.register_factory::<MockFactory>()?;
        let definitions = registry.list_definitions();
        let mut snapshot = registry.clone();

        assert!(matches!(
            snapshot.register_factory::<MockFactory>(),
            Err(ProviderError::FactoryAlreadyRegistered(type_id)) if type_id == "mock"
        ));
        assert_eq!(registry.list_definitions(), definitions);
        assert_eq!(snapshot.list_definitions(), definitions);

        for (target, type_id) in [(&mut registry, "original"), (&mut snapshot, "snapshot")] {
            target.register_dynamic_factory(
                type_id,
                simple_provider_definition(type_id, type_id, false, type_id),
                ProviderPricingPolicy::default(),
                |id, _config| Ok(Box::new(MockProvider::new(id.as_str()))),
                |_| Vec::new(),
                |_| Vec::new(),
            )?;
        }
        assert!(registry.has_factory("original"));
        assert!(!registry.has_factory("snapshot"));
        assert!(snapshot.has_factory("snapshot"));
        assert!(!snapshot.has_factory("original"));
        for target in [&registry, &snapshot] {
            let provider =
                target.create_provider("mock", ProviderId::new("shared"), DynamicConfig::new())?;
            assert_eq!(provider.get_provider_id(), ProviderId::new("shared"));
        }
        Ok(())
    }

    #[test]
    fn test_dynamic_registry_registration() -> Result<()> {
        let mut registry = ProviderRegistry::default();
        let create_count = Arc::new(AtomicUsize::new(0));
        let create_count_for_factory = Arc::clone(&create_count);

        registry.register_dynamic_factory(
            "dynamic-mock",
            simple_provider_definition(
                "Dynamic Mock",
                "Mock provider built from closures",
                true,
                "dynamic-mock",
            ),
            ProviderPricingPolicy::default(),
            move |id, _config| {
                create_count_for_factory.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(MockProvider::new(id.as_str())))
            },
            |_| Vec::new(),
            |_| Vec::new(),
        )?;

        let provider = registry.create_provider(
            "dynamic-mock",
            ProviderId::new("dynamic-mock"),
            DynamicConfig::new(),
        )?;
        assert_eq!(provider.get_provider_id(), ProviderId::new("dynamic-mock"));
        assert_eq!(create_count.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn test_dynamic_registry_rejects_duplicates() -> Result<()> {
        let mut registry = ProviderRegistry::default();
        registry.register_dynamic_factory(
            "duplicate",
            simple_provider_definition("Duplicate", "duplicate", false, "duplicate"),
            ProviderPricingPolicy::default(),
            |id, _config| Ok(Box::new(MockProvider::new(id.as_str()))),
            |_| Vec::new(),
            |_| Vec::new(),
        )?;

        let result = registry.register_dynamic_factory(
            "duplicate",
            simple_provider_definition("Duplicate", "duplicate", false, "duplicate"),
            ProviderPricingPolicy::default(),
            |id, _config| Ok(Box::new(MockProvider::new(id.as_str()))),
            |_| Vec::new(),
            |_| Vec::new(),
        );
        let Err(error) = result else {
            return Err(eyre!("duplicate factory registration succeeded"));
        };

        assert!(matches!(
            error,
            ProviderError::FactoryAlreadyRegistered(provider_type) if provider_type == "duplicate"
        ));
        Ok(())
    }
}
