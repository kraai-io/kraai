use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use kraai_provider_core::{
    Model, ModelConfig, Provider, ProviderDefinition, ProviderError, ProviderPricingPolicy,
    ProviderRequest, ProviderRequestContext, ProviderStreamEvent,
};
use kraai_types::{ModelId, ProviderId};

use super::harness::{RuntimeTestHarness, TEST_TIMEOUT};
use crate::Event;

struct CatalogProvider {
    id: ProviderId,
    fail_cache: bool,
}

#[async_trait::async_trait]
impl Provider for CatalogProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<Model> {
        vec![Model {
            id: ModelId::new("available"),
            name: "Available model".into(),
            max_context: Some(5_000),
            supports_images: false,
            options: Vec::new(),
        }]
    }

    async fn cache_models(&self) -> Result<()> {
        if self.fail_cache {
            Err(eyre!("discovery failed"))
        } else {
            Ok(())
        }
    }

    async fn register_model(&mut self, _: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        _: &ModelId,
        _: ProviderRequest,
        _: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

#[tokio::test]
async fn installed_provider_catalog_rebinds_even_when_discovery_fails() -> Result<()> {
    let Some(mut harness) = RuntimeTestHarness::new(Vec::new()).await else {
        return Ok(());
    };
    for (type_id, fail_cache, fail_creation) in [
        ("catalog-success", false, false),
        ("catalog-discovery-failure", true, false),
        ("catalog-creation-failure", false, true),
    ] {
        harness.runtime.provider_registry.register_dynamic_factory(
            type_id,
            ProviderDefinition {
                type_id: type_id.into(),
                display_name: type_id.into(),
                protocol_family: "test".into(),
                description: String::new(),
                provider_fields: Vec::new(),
                model_fields: Vec::new(),
                supports_model_discovery: true,
                default_provider_id_prefix: type_id.into(),
            },
            ProviderPricingPolicy::default(),
            move |id, _| {
                if fail_creation {
                    Err(ProviderError::ConfigParseError("creation failed".into()))
                } else {
                    Ok(Box::new(CatalogProvider { id, fail_cache }))
                }
            },
            |_| Vec::new(),
            |_| Vec::new(),
        )?;
    }
    let forwarder = harness.runtime.spawn_model_catalog_forwarder();
    harness.maintenance_tasks.push(forwarder);
    let mut sources = harness.runtime.model_catalog_tx.subscribe();
    let mut previous = sources.borrow().clone();
    for (type_id, discovery_failed) in [
        ("catalog-success", false),
        ("catalog-discovery-failure", true),
    ] {
        let mut events = harness.handle.subscribe();
        tokio::fs::write(
            &harness.runtime.config.provider_config_path,
            format!("[[provider]]\nid = \"{type_id}\"\ntype = \"{type_id}\"\n"),
        )
        .await?;
        let result = harness.runtime.load_providers_config_and_emit().await;
        assert_eq!(result.is_err(), discovery_failed);
        assert!(sources.has_changed()?);
        let current = sources.borrow_and_update().clone();
        assert!(!current.same_channel(&previous));
        let installed = harness
            .runtime
            .agent_manager
            .read()
            .await
            .cloned_provider_manager();
        assert!(current.same_channel(&installed.subscribe_model_catalog()));
        assert!(installed.has_provider(&ProviderId::new(type_id)));
        assert_eq!(
            harness.handle.list_models().await?[type_id]
                .first()
                .and_then(|model| model.max_context),
            Some(5_000)
        );
        let mut config_loaded = false;
        tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                match events.recv().await?.event {
                    Event::ConfigLoaded => config_loaded = true,
                    Event::ModelsUpdated => return Ok::<_, color_eyre::Report>(()),
                    _ => {}
                }
            }
        })
        .await??;
        while let Ok(event) = events.try_recv() {
            config_loaded |= matches!(event.event, Event::ConfigLoaded);
        }
        assert_eq!(config_loaded, !discovery_failed);
        previous = current;
    }
    tokio::fs::write(
        &harness.runtime.config.provider_config_path,
        "[[provider]]\nid = \"invalid\"\ntype = \"catalog-creation-failure\"\n",
    )
    .await?;
    assert!(
        harness
            .runtime
            .load_providers_config_and_emit()
            .await
            .is_err()
    );
    assert!(!sources.has_changed()?);
    assert!(sources.borrow().same_channel(&previous));
    harness.shutdown().await;
    Ok(())
}
