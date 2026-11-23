use super::*;

use crate::test_support::{MockFactory, MockProvider, simple_provider_definition};

#[test]
fn pricing_fields_are_only_exposed_for_metered_providers() {
    let mut registry = ProviderRegistry::default();
    for (type_id, subscription) in [("flat-rate", true), ("metered", false)] {
        registry
            .register_dynamic_factory(
                type_id,
                simple_provider_definition("Provider", "Provider", false, type_id),
                ProviderPricingPolicy {
                    subscription,
                    ..ProviderPricingPolicy::default()
                },
                |id, _config| Ok(Box::new(MockProvider::new(id.as_str()))),
                |_| Vec::new(),
                |_| Vec::new(),
            )
            .unwrap();
        let definition = registry
            .get_definition(type_id)
            .expect("missing definition");
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
}

#[test]
fn cloned_registries_isolate_registration_and_rejected_duplicates() {
    let mut registry = ProviderRegistry::default();
    registry.register_factory::<MockFactory>().unwrap();
    let definitions = registry.list_definitions();
    let mut snapshot = registry.clone();

    assert!(matches!(
        snapshot.register_factory::<MockFactory>(),
        Err(ProviderError::FactoryAlreadyRegistered(type_id)) if type_id == "mock"
    ));
    assert_eq!(registry.list_definitions(), definitions);
    assert_eq!(snapshot.list_definitions(), definitions);

    for (target, type_id) in [(&mut registry, "original"), (&mut snapshot, "snapshot")] {
        target
            .register_dynamic_factory(
                type_id,
                simple_provider_definition(type_id, type_id, false, type_id),
                ProviderPricingPolicy::default(),
                |id, _config| Ok(Box::new(MockProvider::new(id.as_str()))),
                |_| Vec::new(),
                |_| Vec::new(),
            )
            .unwrap();
    }
    assert!(registry.has_factory("original"));
    assert!(!registry.has_factory("snapshot"));
    assert!(snapshot.has_factory("snapshot"));
    assert!(!snapshot.has_factory("original"));
    for target in [&registry, &snapshot] {
        let provider = target
            .create_provider("mock", ProviderId::new("shared"), DynamicConfig::new())
            .unwrap();
        assert_eq!(provider.get_provider_id(), ProviderId::new("shared"));
    }
}
