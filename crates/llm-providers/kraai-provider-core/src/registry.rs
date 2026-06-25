use std::collections::BTreeMap;
use std::sync::Arc;

use color_eyre::Result;
use kraai_types::ProviderId;

use crate::config::DynamicConfig;
use crate::definition::{ProviderDefinition, ValidationError};
use crate::error::ProviderError;
use crate::provider::Provider;

#[derive(Default, Clone)]
pub struct ProviderRegistry {
    factories: BTreeMap<String, Arc<FactoryEntry>>,
}

struct FactoryEntry {
    definition: ProviderDefinition,
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

        let entry = FactoryEntry {
            definition,
            create: Arc::new(create),
            validate_provider_config: Arc::new(validate_provider_config),
            validate_model_config: Arc::new(validate_model_config),
        };

        self.factories.insert(key, Arc::new(entry));
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

    pub fn validate_provider_config(
        &self,
        type_id: &str,
        config: &DynamicConfig,
    ) -> Result<Vec<ValidationError>, ProviderError> {
        let entry = self
            .factories
            .get(type_id)
            .ok_or_else(|| ProviderError::UnknownProviderType(type_id.to_string()))?;
        Ok((entry.validate_provider_config)(config))
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
        Ok((entry.validate_model_config)(config))
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
            |id, _config| Ok(Box::new(MockProvider::new(id.as_str()))),
            |_| Vec::new(),
            |_| Vec::new(),
        )?;

        let result = registry.register_dynamic_factory(
            "duplicate",
            simple_provider_definition("Duplicate", "duplicate", false, "duplicate"),
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
