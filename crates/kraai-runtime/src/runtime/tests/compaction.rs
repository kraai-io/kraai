use std::time::Duration;

use async_trait::async_trait;
use color_eyre::eyre::Result;
use futures::stream::{self, BoxStream};
use kraai_provider_core::{
    Model, ModelConfig, Provider, ProviderManager, ProviderRequest, ProviderRequestContext,
    ProviderStreamEvent,
};
use kraai_types::{AssistantPhase, ConversationItem, ModelId, ProviderId};
use tokio_util::sync::CancellationToken;

use super::harness::{RuntimeTestHarness, create_session_with_profile};
use crate::Event;

struct TestSummarizer {
    fail: bool,
    entered: CancellationToken,
    dropped: CancellationToken,
}

#[tokio::test]
async fn failed_compaction_preserves_the_underlying_provider_error() -> Result<()> {
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(TestSummarizer {
            fail: true,
            entered: CancellationToken::new(),
            dropped: CancellationToken::new(),
        }),
    );
    let harness = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let (request, providers) = {
        let mut agent = harness.runtime.agent_manager.write().await;
        let previous = agent
            .prepare_start_stream(
                &session_id,
                "old detail ".repeat(9000),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await?;
        agent
            .set_streaming_message_usage(
                &previous.message_id,
                kraai_types::TokenUsage {
                    input_tokens: 32768,
                    ..Default::default()
                },
            )
            .await?;
        agent.complete_message(&previous.message_id).await?;
        agent.clear_active_turn(&session_id);
        let request = agent
            .prepare_start_stream(
                &session_id,
                String::from("continue"),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await?;
        (request, agent.cloned_provider_manager())
    };
    let result = super::super::core::RuntimeCore::drive_stream(
        session_id,
        request,
        providers,
        harness.runtime.agent_manager.clone(),
        harness.runtime.event_tx.clone(),
        harness.runtime.session_state_barrier.clone(),
    )
    .await;
    let super::super::stream_driver::StreamDriveResult::FailedToStart { error } = result else {
        return Err(color_eyre::eyre::eyre!(
            "expected compaction failure, got {result:?}"
        ));
    };
    assert!(error.contains("reported usage has reached the model context limit"));
    assert!(error.contains("summary provider rejected request"));
    harness.shutdown().await;
    Ok(())
}

#[async_trait]
impl Provider for TestSummarizer {
    fn get_provider_id(&self) -> ProviderId {
        ProviderId::new("mock")
    }

    async fn list_models(&self) -> Vec<Model> {
        vec![Model {
            id: ModelId::new("mock-model"),
            name: String::from("Mock model"),
            max_context: Some(32768),
        }]
    }

    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &ModelId,
        request: ProviderRequest,
        _request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        let is_summary = request.messages.iter().any(|message| {
            matches!(message, ConversationItem::System { text } if text.contains("Summarize the conversation data"))
        });
        if is_summary {
            assert!(request.script_tool.is_none());
            if self.fail {
                return Err(color_eyre::eyre::eyre!("summary provider rejected request"));
            }
            let _drop_guard = self.dropped.clone().drop_guard();
            self.entered.cancel();
            std::future::pending::<()>().await;
        }
        Ok(Box::pin(stream::iter([Ok(
            ProviderStreamEvent::TextDelta {
                item_id: String::from("answer"),
                phase: AssistantPhase::FinalAnswer,
                delta: String::from("done"),
            },
        )])))
    }
}

#[tokio::test]
async fn stalled_compaction_allows_other_sessions_and_cancels_without_losing_history() -> Result<()>
{
    let entered = CancellationToken::new();
    let dropped = CancellationToken::new();
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(TestSummarizer {
            fail: false,
            entered: entered.clone(),
            dropped: dropped.clone(),
        }),
    );
    let harness = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let other_session = create_session_with_profile(&harness.handle, "test-profile").await?;
    {
        let mut agent = harness.runtime.agent_manager.write().await;
        let request = agent
            .prepare_start_stream(
                &session_id,
                "old detail ".repeat(9000),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await?;
        agent
            .set_streaming_message_usage(
                &request.message_id,
                kraai_types::TokenUsage {
                    input_tokens: 27000,
                    ..Default::default()
                },
            )
            .await?;
        agent.complete_message(&request.message_id).await?;
        agent.clear_active_turn(&session_id);
    }
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("continue"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(2), entered.cancelled()).await?;
    tokio::time::timeout(
        Duration::from_secs(2),
        harness.handle.send_message(
            other_session.clone(),
            String::from("hello"),
            String::from("mock-model"),
            String::from("mock"),
        ),
    )
    .await??;
    harness.events.wait_for("other session response", |events| {
        events.iter().any(|event| matches!(event, Event::StreamComplete { session_id, .. } if session_id == &other_session))
    }).await;
    assert!(
        tokio::time::timeout(
            Duration::from_secs(2),
            harness.handle.cancel_stream(session_id.clone()),
        )
        .await??
    );
    tokio::time::timeout(Duration::from_secs(2), dropped.cancelled()).await?;
    harness.events.wait_for("cancelled summary usage", |events| {
        events.iter().any(|event| matches!(event, Event::RequestUsageUpdated { session_id: id, request } if id == &session_id && request.message_id.as_str().starts_with("compaction-")))
    }).await;
    let snapshot = harness.handle.get_session_snapshot(session_id).await?;
    assert!(!snapshot.session.is_running);
    assert!(snapshot.history.values().any(|message| {
        matches!(&message.content, ConversationItem::User { text } if text.len() == 99000)
    }));
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn summary_usage_cannot_cross_a_snapshot_barrier() -> Result<()> {
    let entered = CancellationToken::new();
    let dropped = CancellationToken::new();
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(TestSummarizer {
            fail: false,
            entered: entered.clone(),
            dropped: dropped.clone(),
        }),
    );
    let harness = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let (request, providers) = {
        let mut agent = harness.runtime.agent_manager.write().await;
        let previous = agent
            .prepare_start_stream(
                &session_id,
                "old detail ".repeat(9000),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await?;
        agent
            .set_streaming_message_usage(
                &previous.message_id,
                kraai_types::TokenUsage {
                    input_tokens: 32768,
                    ..Default::default()
                },
            )
            .await?;
        agent.complete_message(&previous.message_id).await?;
        agent.clear_active_turn(&session_id);
        let request = agent
            .prepare_start_stream(
                &session_id,
                String::from("continue"),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
            )
            .await?;
        (request, agent.cloned_provider_manager())
    };
    let snapshot_guard = harness.runtime.session_state_barrier.write().await;
    let task = tokio::spawn(super::super::core::RuntimeCore::drive_stream(
        session_id.clone(),
        request,
        providers,
        harness.runtime.agent_manager.clone(),
        harness.runtime.event_tx.clone(),
        harness.runtime.session_state_barrier.clone(),
    ));
    harness.events.wait_for("compaction preparation", |events| {
        events.iter().any(|event| matches!(event, Event::ContextStateChanged { session_id: id, .. } if id == &session_id))
    }).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), entered.cancelled())
            .await
            .is_err()
    );
    assert!(!harness.events.snapshot().iter().any(|event| {
        matches!(event, Event::RequestUsageUpdated { request, .. } if request.message_id.as_str().starts_with("compaction-"))
    }));
    drop(snapshot_guard);
    tokio::time::timeout(Duration::from_secs(2), entered.cancelled()).await?;
    harness.events.wait_for("summary usage after snapshot", |events| {
        events.iter().any(|event| matches!(event, Event::RequestUsageUpdated { request, .. } if request.message_id.as_str().starts_with("compaction-")))
    }).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), dropped.cancelled()).await?;
    harness.shutdown().await;
    Ok(())
}
