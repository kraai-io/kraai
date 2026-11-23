use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::config::{DynamicConfig, DynamicValue, ProviderConfig};
use crate::test_support::{MockFactory, MockProvider, simple_provider_definition};

#[tokio::test]
async fn loads_registered_provider_config() {
    let mut registry = ProviderRegistry::default();
    registry.register_factory::<MockFactory>().unwrap();

    let mut config = DynamicConfig::new();
    config.insert("token".to_string(), DynamicValue::from("abc"));

    let mut manager = ProviderManager::new();
    manager.register_provider(
        ProviderId::new("active"),
        Box::new(MockProvider::new("active")),
    );
    let snapshot = manager.clone();
    manager
        .load_config(
            ProviderManagerConfig {
                providers: vec![ProviderConfig {
                    id: ProviderId::new("mock"),
                    type_id: "mock".to_string(),
                    config,
                }],
                models: vec![],
            },
            registry,
        )
        .await
        .unwrap();

    assert_eq!(manager.list_providers(), vec![ProviderId::new("mock")]);
    assert_eq!(snapshot.list_providers(), vec![ProviderId::new("active")]);
}

#[test]
fn cloned_managers_isolate_provider_registration() {
    let id = ProviderId::new("shared");
    let mut manager = ProviderManager::new();
    manager.register_provider(id.clone(), Box::new(MockProvider::new("original")));
    let mut snapshot = manager.clone();
    let original = snapshot
        .get_provider(&id)
        .expect("snapshot lost its provider");
    assert!(Arc::ptr_eq(
        &original,
        &manager
            .get_provider(&id)
            .expect("manager lost its provider")
    ));

    manager.register_provider(id.clone(), Box::new(MockProvider::new("replacement")));
    snapshot.register_provider(
        ProviderId::new("snapshot-only"),
        Box::new(MockProvider::new("snapshot-only")),
    );
    assert_eq!(manager.list_providers(), vec![id.clone()]);
    assert_eq!(
        snapshot.list_providers(),
        vec![id.clone(), ProviderId::new("snapshot-only")]
    );
    assert_eq!(
        manager
            .get_provider(&id)
            .expect("manager lost replacement provider")
            .get_provider_id(),
        ProviderId::new("replacement")
    );
    assert!(Arc::ptr_eq(
        &original,
        &snapshot
            .get_provider(&id)
            .expect("snapshot lost original provider")
    ));
}

#[tokio::test]
async fn load_config_bounds_provider_initialization_concurrency() {
    struct ObservedProvider {
        id: ProviderId,
        active_initializations: Arc<AtomicUsize>,
        peak_initializations: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Provider for ObservedProvider {
        fn get_provider_id(&self) -> ProviderId {
            self.id.clone()
        }

        async fn list_models(&self) -> Vec<Model> {
            Vec::new()
        }

        async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
            let active = self.active_initializations.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak_initializations
                .fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(25)).await;
            self.active_initializations.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }

        async fn generate_reply_stream(
            &self,
            _model_id: &ModelId,
            _request: ProviderRequest,
            _request_context: &ProviderRequestContext,
        ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
            unreachable!("not used by this test")
        }
    }

    let active_initializations = Arc::new(AtomicUsize::new(0));
    let peak_initializations = Arc::new(AtomicUsize::new(0));
    let mut registry = ProviderRegistry::default();
    registry
        .register_dynamic_factory(
            "observed",
            simple_provider_definition(
                "Observed",
                "Records initialization concurrency",
                false,
                "observed",
            ),
            crate::ProviderPricingPolicy::default(),
            {
                let active_initializations = active_initializations.clone();
                let peak_initializations = peak_initializations.clone();
                move |id, _config| {
                    Ok(Box::new(ObservedProvider {
                        id,
                        active_initializations: active_initializations.clone(),
                        peak_initializations: peak_initializations.clone(),
                    }))
                }
            },
            |_| Vec::new(),
            |_| Vec::new(),
        )
        .unwrap();

    let provider_count = ProviderManager::PROVIDER_INITIALIZATION_CONCURRENCY + 2;
    let config = ProviderManagerConfig {
        providers: (0..provider_count)
            .map(|index| ProviderConfig {
                id: ProviderId::new(format!("provider-{index}")),
                type_id: "observed".to_string(),
                config: DynamicConfig::new(),
            })
            .collect(),
        models: (0..provider_count)
            .map(|index| ModelConfig {
                options: Vec::new(),
                remove_options: Vec::new(),
                id: ModelId::new(format!("model-{index}")),
                provider_id: ProviderId::new(format!("provider-{index}")),
                config: DynamicConfig::new(),
            })
            .collect(),
    };

    ProviderManager::new()
        .load_config(config, registry)
        .await
        .unwrap();

    assert_eq!(
        peak_initializations.load(Ordering::SeqCst),
        ProviderManager::PROVIDER_INITIALIZATION_CONCURRENCY
    );
}

#[tokio::test]
async fn rejects_invalid_config() {
    let mut registry = ProviderRegistry::default();
    registry.register_factory::<MockFactory>().unwrap();

    let result = ProviderManager::new()
        .load_config(
            ProviderManagerConfig {
                providers: vec![ProviderConfig {
                    id: ProviderId::new("mock"),
                    type_id: "mock".to_string(),
                    config: DynamicConfig::new(),
                }],
                models: vec![],
            },
            registry,
        )
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn load_config_preserves_active_providers_when_creation_fails() {
    let mut registry = ProviderRegistry::default();
    registry.register_factory::<MockFactory>().unwrap();
    registry
        .register_dynamic_factory(
            "failing",
            simple_provider_definition("Failing", "Fails during creation", false, "failing"),
            crate::ProviderPricingPolicy::default(),
            |_id, _config| {
                Err(ProviderError::ConfigParseError(
                    "provider creation failed".to_string(),
                ))
            },
            |_| Vec::new(),
            |_| Vec::new(),
        )
        .unwrap();

    let mut valid_config = DynamicConfig::new();
    valid_config.insert("token".to_string(), DynamicValue::from("abc"));

    let mut manager = ProviderManager::new();
    manager.register_provider(
        ProviderId::new("active"),
        Box::new(MockProvider::new("active")),
    );
    let snapshot = manager.clone();

    let result = manager
        .load_config(
            ProviderManagerConfig {
                providers: vec![
                    ProviderConfig {
                        id: ProviderId::new("valid"),
                        type_id: "mock".to_string(),
                        config: valid_config,
                    },
                    ProviderConfig {
                        id: ProviderId::new("invalid"),
                        type_id: "failing".to_string(),
                        config: DynamicConfig::new(),
                    },
                ],
                models: vec![],
            },
            registry,
        )
        .await;

    assert!(result.is_err());
    assert_eq!(manager.list_providers(), vec![ProviderId::new("active")]);
    assert_eq!(snapshot.list_providers(), vec![ProviderId::new("active")]);
}
