use color_eyre::Result;
use futures::stream::BoxStream;
use kraai_types::{ChatMessage, ModelId, ProviderId};
use serde::{Deserialize, Serialize};

use crate::config::ModelConfig;
use crate::http_retry::ProviderRequestContext;
use crate::stream::ProviderStreamEvent;

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn get_provider_id(&self) -> ProviderId;

    async fn list_models(&self) -> Vec<Model>;

    async fn cache_models(&self) -> Result<()>;

    async fn register_model(&mut self, model: ModelConfig) -> Result<()>;

    async fn generate_reply(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
        request_context: &ProviderRequestContext,
    ) -> Result<ChatMessage>;

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
        request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: ModelId,
    pub name: String,
    pub max_context: Option<usize>,
}
