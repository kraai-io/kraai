use color_eyre::eyre::{Result, bail, ensure};
use futures::stream::{self, BoxStream};
use kraai_provider_core::{
    Model, ModelConfig, Provider, ProviderManager, ProviderRequest, ProviderRequestContext,
    ProviderStreamEvent,
};
use kraai_types::{AssistantPhase, ModelId, ProviderId};

pub(crate) const CHUNKS: usize = 2_048;
pub(crate) const CHUNK_BYTES: usize = 128;

pub(crate) fn provider_id() -> ProviderId {
    ProviderId::new("performance-offline")
}

pub(crate) fn model_id() -> ModelId {
    ModelId::new("performance-fixture")
}

pub(crate) fn manager() -> ProviderManager {
    let mut manager = ProviderManager::new();
    manager.register_provider(provider_id(), Box::new(FixtureProvider));
    manager
}

pub(crate) fn chunk(index: usize) -> String {
    let prefix = format!("chunk-{index:04} ");
    format!("{prefix}{}\n", "x".repeat(CHUNK_BYTES - prefix.len() - 1))
}

struct FixtureProvider;

#[async_trait::async_trait]
impl Provider for FixtureProvider {
    fn get_provider_id(&self) -> ProviderId {
        provider_id()
    }

    async fn list_models(&self) -> Vec<Model> {
        vec![Model {
            id: model_id(),
            name: "Offline performance fixture".into(),
            max_context: None,
            supports_images: false,
            options: Vec::new(),
        }]
    }

    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        bail!("The performance fixture has a fixed model")
    }

    async fn generate_reply_stream(
        &self,
        selected_model: &ModelId,
        request: ProviderRequest,
        _request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        ensure!(
            *selected_model == model_id(),
            "Wrong offline model selected"
        );
        ensure!(
            request
                .script_tool
                .is_some_and(|tool| tool.name == "kraai_nushell"),
            "Agent request is missing its script tool"
        );
        ensure!(
            !request.messages.is_empty(),
            "Agent request has no messages"
        );
        Ok(Box::pin(stream::iter((0..CHUNKS).map(|index| {
            Ok(ProviderStreamEvent::TextDelta {
                item_id: "fixture-response".into(),
                phase: AssistantPhase::FinalAnswer,
                delta: chunk(index),
            })
        }))))
    }
}
