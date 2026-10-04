use color_eyre::Result;
use futures::stream::BoxStream;
use kraai_types::{ConversationItem, ModelId, ProviderId};
use serde::{Deserialize, Serialize};

use crate::config::ModelConfig;
use crate::request_context::ProviderRequestContext;
use crate::stream::ProviderStreamEvent;

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn get_provider_id(&self) -> ProviderId;

    async fn pricing_model_id(&self, model_id: &ModelId) -> Result<ModelId> {
        Ok(model_id.clone())
    }

    async fn list_models(&self) -> Vec<Model>;

    async fn get_model(&self, model_id: &ModelId) -> Option<Model> {
        self.list_models()
            .await
            .into_iter()
            .find(|model| model.id == *model_id)
    }

    fn set_model_catalog(&mut self, _catalog: std::sync::Arc<crate::ModelCatalog>) {}

    async fn cache_models(&self) -> Result<()>;

    async fn register_model(&mut self, model: ModelConfig) -> Result<()>;

    fn cache_warming_policy(&self, _model_id: &ModelId) -> Option<crate::CacheWarmingPolicy> {
        None
    }

    fn supports_native_compaction(&self, _model_id: &ModelId) -> bool {
        false
    }

    async fn compact_stream(
        &self,
        _model_id: &ModelId,
        _request: ProviderRequest,
        _request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        color_eyre::eyre::bail!("Provider does not support native compaction")
    }

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        request: ProviderRequest,
        request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptToolDefinition {
    pub name: String,
    pub description: String,
}

impl ScriptToolDefinition {
    pub const NAME: &str = "kraai_nushell";

    pub fn nushell() -> Self {
        Self {
            name: Self::NAME.to_string(),
            description: "Execute a Nushell script. Supply the complete script, beginning with a metadata comment containing timeout, such as # timeout=30sec.".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub messages: Vec<ConversationItem>,
    pub script_tool: Option<ScriptToolDefinition>,
    pub cacheable_messages: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: ModelId,
    pub name: String,
    pub max_context: Option<usize>,
    pub supports_images: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::eyre::{ensure, eyre};

    #[tokio::test]
    async fn default_model_lookup_preserves_listed_metadata_and_missing_ids() -> Result<()> {
        let provider = crate::test_support::MockProvider::new("fixture");
        let listed = provider
            .list_models()
            .await
            .into_iter()
            .next()
            .ok_or_else(|| eyre!("fixture model missing"))?;
        let found = provider
            .get_model(&listed.id)
            .await
            .ok_or_else(|| eyre!("model lookup missed listed model"))?;
        ensure!(found.id == listed.id);
        ensure!(found.name == listed.name);
        ensure!(found.max_context == listed.max_context);
        ensure!(provider.get_model(&ModelId::new("unknown")).await.is_none());
        Ok(())
    }
}
