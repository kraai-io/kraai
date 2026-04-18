use super::super::*;
use async_trait::async_trait;
use color_eyre::eyre::Result;
use futures::stream::BoxStream;
use kraai_provider_core::Provider;
use kraai_tool_core::{ToolCallResult, ToolContext, TypedTool};
use kraai_types::{ChatMessage as ProviderChatMessage, ToolStateDelta};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) struct MockProvider {
    id: ProviderId,
}

#[derive(Clone, Deserialize)]
pub(super) struct MockToolArgs {}

#[derive(Clone)]
pub(super) struct MockTool {
    pub(super) name: &'static str,
}

#[async_trait]
impl TypedTool for MockTool {
    type Args = MockToolArgs;

    fn name(&self) -> &'static str {
        self.name
    }

    fn schema(&self) -> &'static str {
        "mock schema"
    }

    async fn call(&self, _args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
        ToolCallResult::success(serde_json::json!({ "ok": true }))
    }
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

    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(MockProvider {
            id: ProviderId::new("mock"),
        }),
    );

    let mut tools = ToolManager::new();
    tools.register_tool(MockTool { name: "close_file" });
    tools.register_tool(MockTool { name: "list_files" });
    tools.register_tool(MockTool { name: "open_file" });
    tools.register_tool(MockTool {
        name: "search_files",
    });
    tools.register_tool(MockTool { name: "read_files" });
    tools.register_tool(MockTool { name: "edit_file" });

    let manager = AgentManager::new(
        providers,
        tools,
        PathBuf::from("/tmp/default-workspace"),
        message_store,
        session_store,
    );
    (manager, data_dir)
}

pub(super) async fn cleanup_dir(data_dir: PathBuf) {
    let _ = tokio::fs::remove_dir_all(data_dir).await;
}

pub(super) fn open_file_state_delta(path: &Path) -> ToolStateDelta {
    ToolStateDelta {
        namespace: String::from("opened_files"),
        operation: String::from("open"),
        payload: json!({ "path": path.display().to_string() }),
    }
}
