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

use super::harness::{RuntimeTestHarness, TEST_TIMEOUT, create_session_with_profile};
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
        let agent = &harness.runtime.agent_manager;
        let previous = agent
            .prepare_start_stream(
                &session_id,
                "old detail ".repeat(9000).into(),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
                Default::default(),
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
                String::from("continue").into(),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
                Default::default(),
            )
            .await?;
        (request, agent.cloned_provider_manager())
    };
    let result = super::harness::drive_stream(
        harness.runtime.clone(),
        session_id.clone(),
        providers,
        request,
    )
    .await;
    let super::super::stream_driver::StreamDriveResult::FailedToStart { error } = result else {
        return Err(color_eyre::eyre::eyre!(
            "expected compaction failure, got {result:?}"
        ));
    };
    assert!(error.contains("Context compaction failed"));
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
            options: Default::default(),
            supports_images: true,
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
            matches!(message, ConversationItem::User { content } if content.display_text().contains("CONTEXT CHECKPOINT COMPACTION"))
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
        let agent = &harness.runtime.agent_manager;
        let request = agent
            .prepare_start_stream(
                &session_id,
                "old detail ".repeat(9000).into(),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
                Default::default(),
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
            Default::default(),
        )
        .await?;
    tokio::time::timeout(TEST_TIMEOUT, entered.cancelled()).await?;
    tokio::time::timeout(
        TEST_TIMEOUT,
        harness.handle.send_message(
            other_session.clone(),
            String::from("hello"),
            String::from("mock-model"),
            String::from("mock"),
            Default::default(),
        ),
    )
    .await??;
    harness.events.wait_for("other session response", |events| {
        events.iter().any(|event| matches!(event, Event::StreamComplete { session_id, .. } if session_id == &other_session))
    }).await;
    assert!(
        tokio::time::timeout(
            TEST_TIMEOUT,
            harness.handle.cancel_stream(session_id.clone()),
        )
        .await??
    );
    tokio::time::timeout(TEST_TIMEOUT, dropped.cancelled()).await?;
    harness.events.wait_for("cancelled summary usage", |events| {
        events.iter().any(|event| matches!(event, Event::RequestUsageUpdated { session_id: id, request } if id == &session_id && request.message_id.as_str().starts_with("compaction-")))
    }).await;
    let snapshot = harness.handle.get_session_snapshot(session_id).await?;
    assert!(!snapshot.session.is_running);
    assert!(snapshot.history.values().any(|message| {
        matches!(&message.content, ConversationItem::User { content: text } if text.display_text().len() == 99000)
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
        let agent = &harness.runtime.agent_manager;
        let previous = agent
            .prepare_start_stream(
                &session_id,
                "old detail ".repeat(9000).into(),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
                Default::default(),
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
                String::from("continue").into(),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
                Default::default(),
            )
            .await?;
        (request, agent.cloned_provider_manager())
    };
    let barrier = harness.runtime.sessions.barrier(&session_id);
    let snapshot_guard = barrier.write().await;
    let task = tokio::spawn(super::harness::drive_stream(
        harness.runtime.clone(),
        session_id.clone(),
        providers,
        request,
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
    tokio::time::timeout(TEST_TIMEOUT, entered.cancelled()).await?;
    harness.events.wait_for("summary usage after snapshot", |events| {
        events.iter().any(|event| matches!(event, Event::RequestUsageUpdated { request, .. } if request.message_id.as_str().starts_with("compaction-")))
    }).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(TEST_TIMEOUT, dropped.cancelled()).await?;
    harness.shutdown().await;
    Ok(())
}

/// Rejects conversation requests that were not compacted yet as too large for the context.
struct OverflowingProvider {
    requests: std::sync::Mutex<Vec<&'static str>>,
    in_stream: bool,
    always_overflow: bool,
}

impl OverflowingProvider {
    fn overflow() -> color_eyre::Report {
        kraai_provider_core::ProviderError::ContextWindowExceeded(String::from("too long")).into()
    }
}

#[async_trait]
impl Provider for OverflowingProvider {
    fn get_provider_id(&self) -> ProviderId {
        ProviderId::new("mock")
    }

    async fn list_models(&self) -> Vec<Model> {
        vec![super::harness::mock_model()]
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
        let text = serde_json::to_string(&request.messages)?;
        let kind = if text.contains("CONTEXT CHECKPOINT COMPACTION") {
            "summary"
        } else if text.contains("summary of the work") {
            "compacted"
        } else {
            "original"
        };
        self.requests
            .lock()
            .map_err(|error| color_eyre::eyre::eyre!("{error}"))?
            .push(kind);
        let delta = |text: &str| {
            Ok(ProviderStreamEvent::TextDelta {
                item_id: String::from("answer"),
                phase: AssistantPhase::FinalAnswer,
                delta: text.into(),
            })
        };
        match kind {
            "summary" => Ok(Box::pin(stream::iter([delta("summary of the work")]))),
            "compacted" if !self.always_overflow => Ok(Box::pin(stream::iter([delta("done")]))),
            _ if self.in_stream => Ok(Box::pin(stream::iter([
                delta("partial"),
                Err(Self::overflow()),
            ]))),
            _ => Err(Self::overflow()),
        }
    }
}

async fn drive_overflowing(
    in_stream: bool,
    always_overflow: bool,
) -> Result<(
    super::super::stream_driver::StreamDriveResult,
    Vec<&'static str>,
)> {
    let provider = std::sync::Arc::new(OverflowingProvider {
        requests: std::sync::Mutex::default(),
        in_stream,
        always_overflow,
    });
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(SharedProvider(provider.clone())),
    );
    let harness = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let agent = &harness.runtime.agent_manager;
    let request = agent
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await?;
    assert!(!request.compact_first);
    let result = super::harness::drive_stream(
        harness.runtime.clone(),
        session_id,
        agent.cloned_provider_manager(),
        request,
    )
    .await;
    harness.shutdown().await;
    let requests = provider
        .requests
        .lock()
        .map_err(|error| color_eyre::eyre::eyre!("{error}"))?
        .clone();
    Ok((result, requests))
}

struct SharedProvider(std::sync::Arc<OverflowingProvider>);

#[async_trait]
impl Provider for SharedProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.0.get_provider_id()
    }

    async fn list_models(&self) -> Vec<Model> {
        self.0.list_models().await
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
        request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        self.0
            .generate_reply_stream(model_id, request, request_context)
            .await
    }
}

#[tokio::test]
async fn context_overflow_compacts_once_and_retries_the_request() -> Result<()> {
    for in_stream in [false, true] {
        let (result, requests) = drive_overflowing(in_stream, false).await?;
        assert!(
            matches!(
                result,
                super::super::stream_driver::StreamDriveResult::Completed(_)
            ),
            "{result:?}"
        );
        assert_eq!(requests, ["original", "summary", "compacted"]);
    }
    let (result, requests) = drive_overflowing(false, true).await?;
    let super::super::stream_driver::StreamDriveResult::FailedToStart { error } = result else {
        return Err(color_eyre::eyre::eyre!(
            "expected the second overflow to fail, got {result:?}"
        ));
    };
    assert!(error.contains("too long"));
    assert_eq!(requests, ["original", "summary", "compacted"]);
    Ok(())
}
