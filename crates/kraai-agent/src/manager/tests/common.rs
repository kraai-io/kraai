use super::super::*;
use color_eyre::eyre::Result;
use futures::stream::BoxStream;
use kraai_provider_core::{Provider, ProviderRequest};
use std::path::PathBuf;
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
            supports_images: true,
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

    async fn generate_reply_stream(
        &self,
        _model_id: &ModelId,
        _request: ProviderRequest,
        _request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<kraai_provider_core::ProviderStreamEvent>>> {
        Ok(Box::pin(futures::stream::iter(vec![Ok(
            kraai_provider_core::ProviderStreamEvent::TextDelta {
                item_id: String::from("mock-message"),
                phase: AssistantPhase::FinalAnswer,
                delta: String::from("reply"),
            },
        )])))
    }
}

pub(super) fn test_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("agent-core-{name}-{nanos}-{}", Ulid::generate()))
}

pub(super) async fn test_manager() -> (AgentManager, PathBuf) {
    let data_dir = test_dir("manager");
    tokio::fs::create_dir_all(&data_dir).await.unwrap();

    let persistence = kraai_persistence::Persistence::open(&data_dir)
        .await
        .unwrap();

    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(MockProvider {
            id: ProviderId::new("mock"),
        }),
    );
    providers.register_provider(
        ProviderId::new("mock-alternate"),
        Box::new(MockProvider {
            id: ProviderId::new("mock-alternate"),
        }),
    );

    let manager = AgentManager::new(
        providers,
        PathBuf::from("/tmp/default-workspace"),
        persistence,
        data_dir.clone(),
    );
    (manager, data_dir)
}

pub(super) async fn cleanup_dir(data_dir: PathBuf) {
    let _ = tokio::fs::remove_dir_all(data_dir).await;
}

pub(super) fn corrupt_message(data_dir: &Path, id: &MessageId, data: &str) -> Result<()> {
    rusqlite::Connection::open(data_dir.join("kraai.sqlite3"))?.execute(
        "INSERT INTO records(kind, id, data) VALUES ('message', ?1, ?2) ON CONFLICT(kind, id) DO UPDATE SET data = excluded.data",
        rusqlite::params![id.as_str(), data],
    )?;
    Ok(())
}
