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

    async fn list_models(&self) -> Vec<Model>;

    async fn get_model(&self, model_id: &ModelId) -> Option<Model> {
        self.list_models()
            .await
            .into_iter()
            .find(|model| model.id == *model_id)
    }

    fn set_model_catalog(&mut self, _catalog: std::sync::Arc<crate::ModelCatalog>) {}

    fn model_list_source(&self) -> Option<String> {
        None
    }

    async fn fetch_model_list(&self) -> Result<serde_json::Value> {
        color_eyre::eyre::bail!("Provider does not discover models")
    }

    async fn install_model_list(&self, _list: serde_json::Value) -> Result<()> {
        Ok(())
    }

    async fn refresh_model_list(&self) -> Result<()> {
        let list = self.fetch_model_list().await?;
        self.install_model_list(list).await
    }

    async fn register_model(&mut self, model: ModelConfig) -> Result<()>;

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
            description: "Execute a complete Nushell script.".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub messages: Vec<ConversationItem>,
    pub script_tool: Option<ScriptToolDefinition>,
    pub options: kraai_types::ModelOptionValues,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: ModelId,
    pub name: String,
    pub max_context: Option<usize>,
    #[serde(default)]
    pub max_output: Option<usize>,
    pub supports_images: bool,
    #[serde(default)]
    pub options: Vec<kraai_types::ModelOptionDefinition>,
}
