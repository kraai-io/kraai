use std::time::Duration;

use color_eyre::eyre::Result;
use futures::stream::{self, BoxStream};
use kraai_provider_core::{
    Model, ModelConfig, Provider, ProviderManager, ProviderRequest, ProviderRequestContext,
    ProviderStreamEvent,
};
use kraai_types::{AssistantPhase, ChatRole, ModelId, ProviderId};
use tokio::sync::mpsc;

use super::harness::{RuntimeTestHarness, create_session_with_profile};
use crate::ContinueSessionOutcome;

struct RecordingProvider {
    id: ProviderId,
    requests: mpsc::UnboundedSender<(ModelId, ProviderRequest)>,
}

#[async_trait::async_trait]
impl Provider for RecordingProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<Model> {
        Vec::new()
    }

    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        request: ProviderRequest,
        _context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        self.requests.send((model_id.clone(), request))?;
        Ok(Box::pin(stream::pending()))
    }
}

#[tokio::test]
async fn explicit_continuation_uses_selected_model_without_appending_user_message() -> Result<()> {
    assert_selected_model(false).await
}

#[tokio::test]
async fn explicit_continuation_uses_selected_model_when_consuming_queued_messages() -> Result<()> {
    assert_selected_model(true).await
}

async fn assert_selected_model(queued: bool) -> Result<()> {
    let (requests, mut received) = mpsc::unbounded_channel();
    let mut providers = ProviderManager::new();
    for id in ["old-provider", "selected-provider"] {
        providers.register_provider(
            ProviderId::new(id),
            Box::new(RecordingProvider {
                id: ProviderId::new(id),
                requests: requests.clone(),
            }),
        );
    }
    let harness = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("runtime fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let original_tip = {
        let mut agent = harness.runtime.agent_manager.write().await;
        let first = agent
            .prepare_start_stream(
                &session_id,
                "first message".into(),
                ModelId::new("old-model"),
                ProviderId::new("old-provider"),
            )
            .await?;
        agent
            .append_text_chunk(
                &first.message_id,
                "answer",
                AssistantPhase::FinalAnswer,
                "first answer",
            )
            .await;
        agent.complete_message(&first.message_id).await?;
        agent.clear_active_turn(&session_id);
        drop(agent);
        first.message_id
    };
    if queued {
        harness
            .runtime
            .restore_queued_messages(
                &session_id,
                vec![super::super::core::QueuedMessage {
                    message: "queued message".into(),
                    model_id: ModelId::new("old-model"),
                    provider_id: ProviderId::new("old-provider"),
                }],
            )
            .await;
    }
    assert_eq!(
        harness
            .handle
            .continue_session(
                session_id.clone(),
                "selected-model".into(),
                "selected-provider".into(),
            )
            .await?,
        ContinueSessionOutcome::Started
    );
    let (model, request) = tokio::time::timeout(Duration::from_secs(1), received.recv())
        .await?
        .expect("provider request");
    assert_eq!(model.as_str(), "selected-model");
    assert!(request.script_tool.is_some());
    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert_eq!(
        history
            .values()
            .filter(|message| message.role() == ChatRole::User)
            .count(),
        if queued { 2 } else { 1 }
    );
    assert!(
        request
            .messages
            .contains(&history.get(&original_tip).expect("original answer").content)
    );
    let tip = harness
        .runtime
        .agent_manager
        .read()
        .await
        .get_tip(&session_id)
        .await?
        .expect("tip");
    let message = history.get(&tip).expect("continued assistant");
    if queued {
        let parent = history
            .get(message.parent_id.as_ref().expect("queued message parent"))
            .expect("queued message");
        assert_eq!(parent.content.text(), Some("queued message"));
        assert_eq!(parent.parent_id.as_ref(), Some(&original_tip));
    } else {
        assert_eq!(message.parent_id.as_ref(), Some(&original_tip));
    }
    let generation = message.generation.as_ref().expect("generation metadata");
    assert_eq!(generation.model_id.as_str(), "selected-model");
    assert_eq!(generation.provider_id.as_str(), "selected-provider");
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn continuing_empty_session_does_not_start_a_turn() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    assert_eq!(
        harness
            .handle
            .continue_session(session_id.clone(), "mock-model".into(), "mock".into())
            .await?,
        ContinueSessionOutcome::NothingToContinue
    );
    let agent = harness.runtime.agent_manager.read().await;
    assert!(!agent.is_turn_active(&session_id));
    assert!(agent.get_tip(&session_id).await?.is_none());
    drop(agent);
    harness.shutdown().await;
    Ok(())
}
