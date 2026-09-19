use color_eyre::Result;
use futures::stream::BoxStream;
use kraai_types::{ConversationItem, ModelId, ProviderId};
use serde::{Deserialize, Serialize};

use crate::config::ModelConfig;
use crate::http_retry::ProviderRequestContext;
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

    async fn cache_models(&self) -> Result<()>;

    async fn register_model(&mut self, model: ModelConfig) -> Result<()>;

    fn script_tool_transport(&self, _model_id: &ModelId) -> ScriptToolTransport {
        ScriptToolTransport::TextEnvelope
    }

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        request: ProviderRequest,
        request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptToolTransport {
    TextEnvelope,
    NativeCustom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptToolDefinition {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub messages: Vec<ConversationItem>,
    pub script_tool: Option<ScriptToolDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: ModelId,
    pub name: String,
    pub max_context: Option<usize>,
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
