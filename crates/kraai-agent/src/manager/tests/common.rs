use super::super::*;
use color_eyre::eyre::Result;
use futures::stream::BoxStream;
use kraai_provider_core::Provider;
use kraai_types::ChatMessage as ProviderChatMessage;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) struct MockProvider {
    id: ProviderId,
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<Model> {
        vec![Model {
            id: ModelId::new("mock-model"),
            name: String::from("Mock Model"),
            max_context: None,
        }]
    }

    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }

    async fn register_model(&mut self, _model: kraai_provider_core::ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply(
        &self,
        _model_id: &ModelId,
        _messages: Vec<ProviderChatMessage>,
        _request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<ProviderChatMessage> {
        Ok(ProviderChatMessage {
            role: ChatRole::Assistant,
            content: String::from("reply"),
        })
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &ModelId,
        _messages: Vec<ProviderChatMessage>,
        _request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<kraai_provider_core::ProviderStreamEvent>>> {
        Ok(Box::pin(futures::stream::iter(vec![Ok(
            kraai_provider_core::ProviderStreamEvent::TextDelta(String::from("reply")),
        )])))
    }
}

pub(super) fn test_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("agent-core-{name}-{nanos}-{}", Ulid::new()))
}

pub(super) async fn test_manager() -> (AgentManager, PathBuf) {
    let data_dir = test_dir("manager");
    tokio::fs::create_dir_all(&data_dir).await.unwrap();

    let message_store = Arc::new(kraai_persistence::FileMessageStore::new(&data_dir));
    let session_store = Arc::new(kraai_persistence::FileSessionStore::new(
        &data_dir,
        message_store.clone(),
    ));
    let execution_store = Arc::new(kraai_persistence::FileScriptExecutionStore::new(&data_dir));

    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(MockProvider {
            id: ProviderId::new("mock"),
        }),
    );

    let manager = AgentManager::new(
        providers,
        PathBuf::from("/tmp/default-workspace"),
        message_store,
        session_store,
        execution_store,
    );
    (manager, data_dir)
}

pub(super) async fn cleanup_dir(data_dir: PathBuf) {
    let _ = tokio::fs::remove_dir_all(data_dir).await;
}
