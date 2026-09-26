use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use color_eyre::eyre::{Result, eyre};
use futures::stream::{self, BoxStream};
use kraai_provider_core::{
    CacheWarmingPolicy, Model, ModelConfig, Provider, ProviderManager, ProviderRequest,
    ProviderRequestContext, ProviderStreamEvent, ScriptToolTransport,
};
use kraai_types::{AssistantPhase, ConversationItem, ModelId, ProviderId, TokenUsage, ToolCallId};
use tokio_util::sync::CancellationToken;

use super::super::{core::RuntimeCore, stream_driver::StreamDriveResult};
use super::harness::{RuntimeTestHarness, create_session_with_profile};

#[derive(Clone, Copy)]
enum WarmResult {
    InvalidPolicy,
    Success,
    Failure,
    PartialFailure,
    NoUsage,
    Stall,
    WaitForCancellation,
}

type CapturedRequests = Arc<Mutex<Vec<(ProviderRequest, Option<String>)>>>;

struct WarmingProvider {
    calls: CapturedRequests,
    result: WarmResult,
    entered: CancellationToken,
}

#[async_trait]
impl Provider for WarmingProvider {
    fn get_provider_id(&self) -> ProviderId {
        ProviderId::new("mock")
    }
    async fn list_models(&self) -> Vec<Model> {
        vec![Model {
            supports_images: true,
            id: ModelId::new("mock-model"),
            name: "Mock".into(),
            max_context: None,
        }]
    }
    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }
    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }
    fn script_tool_transport(&self, _model: &ModelId) -> ScriptToolTransport {
        ScriptToolTransport::NativeCustom
    }
    fn cache_warming_policy(&self, _model: &ModelId) -> Option<CacheWarmingPolicy> {
        Some(CacheWarmingPolicy {
            min_requests_between_warmups: if matches!(self.result, WarmResult::InvalidPolicy) {
                0
            } else {
                2
            },
            timeout: if matches!(self.result, WarmResult::Stall) {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(60)
            },
            ..Default::default()
        })
    }
    async fn generate_reply_stream(
        &self,
        _model: &ModelId,
        request: ProviderRequest,
        context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        let first = {
            let mut calls = self.calls.lock().unwrap();
            calls.push((request, context.prompt_cache_key().map(str::to_owned)));
            calls.len() == 1 && !matches!(self.result, WarmResult::InvalidPolicy)
        };
        if first {
            self.entered.cancel();
            match self.result {
                WarmResult::Failure => return Err(eyre!("warming rejected")),
                WarmResult::NoUsage => return Ok(Box::pin(stream::empty())),
                WarmResult::Stall | WarmResult::WaitForCancellation => {
                    return Ok(Box::pin(stream::pending()));
                }
                WarmResult::Success | WarmResult::PartialFailure | WarmResult::InvalidPolicy => {}
            }
            context.retry_observer().unwrap().before_attempt(1).await?;
        }
        let mut events = vec![Ok(ProviderStreamEvent::TextDelta {
            item_id: "reply".into(),
            phase: AssistantPhase::FinalAnswer,
            delta: if first { "discard this" } else { "real answer" }.into(),
        })];
        if first {
            events.push(Ok(ProviderStreamEvent::ScriptCall {
                call_id: ToolCallId::new("discarded-call"),
                name: "kraai_nushell".into(),
                input: "# timeout=10sec\nerror make {msg: 'must never execute'}".into(),
            }));
        }
        events.push(Ok(ProviderStreamEvent::Usage(TokenUsage {
            input_tokens: if first { 2000 } else { 3000 },
            output_tokens: 2,
            ..Default::default()
        })));
        if first && matches!(self.result, WarmResult::PartialFailure) {
            events.push(Err(eyre!("stream failed after usage")));
        }
        Ok(Box::pin(stream::iter(events)))
    }
}

async fn fixture(
    result: WarmResult,
) -> Result<(
    RuntimeTestHarness,
    String,
    kraai_agent::PendingStreamRequest,
    CapturedRequests,
    CancellationToken,
)> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let entered = CancellationToken::new();
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(WarmingProvider {
            calls: calls.clone(),
            result,
            entered: entered.clone(),
        }),
    );
    let harness = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("runtime fixture");
    let session = create_session_with_profile(&harness.handle, "test-profile").await?;
    let mut request = harness
        .runtime
        .agent_manager
        .write()
        .await
        .prepare_start_stream(
            &session,
            "history ".repeat(1500).into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    request.provider_request.cacheable_messages = Some(request.provider_request.messages.len());
    request
        .provider_request
        .messages
        .push(ConversationItem::System {
            text: "fresh pinned files".into(),
        });
    Ok((harness, session, request, calls, entered))
}

#[tokio::test]
async fn warming_discards_outputs_and_accounts_separately_before_the_real_request() -> Result<()> {
    let (harness, session, request, calls, _) = fixture(WarmResult::Success).await?;
    let message_id = request.message_id.clone();
    let providers = harness
        .runtime
        .agent_manager
        .read()
        .await
        .cloned_provider_manager();
    let result = RuntimeCore::drive_stream(
        session.clone(),
        request,
        providers,
        harness.runtime.agent_manager.clone(),
        harness.runtime.event_tx.clone(),
        harness.runtime.session_state_barrier.clone(),
    )
    .await;
    assert!(matches!(result, StreamDriveResult::Completed(output) if output.call_id.is_none()));
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1.as_deref(), Some(session.as_str()));
        assert_eq!(calls[0].1, calls[1].1);
        assert_eq!(calls[0].0.script_tool, calls[1].0.script_tool);
        assert_eq!(
            calls[0].0.messages,
            calls[1].0.messages[..calls[1].0.messages.len() - 1]
        );
    }
    let agent = harness.runtime.agent_manager.read().await;
    let history = agent.get_chat_history(&session).await?;
    assert!(
        matches!(history[&message_id].content.assistant_items(), Some([kraai_types::AssistantItem::Text { text, .. }]) if text == "real answer")
    );
    let requests = agent.request_usage_store().load(&session).await?;
    assert_eq!(requests.len(), 2);
    let warm = requests
        .values()
        .find(|request| request.message_id.as_str().starts_with("cache-warming-"))
        .unwrap();
    assert_eq!(warm.usage.as_ref().unwrap().input_tokens, 2000);
    assert_eq!(warm.unpriced_attempts, 1);
    assert_eq!(
        requests[&message_id].usage.as_ref().unwrap().input_tokens,
        3000
    );
    agent.complete_message(&message_id).await?;
    assert_eq!(
        agent
            .get_session_context_usage(&session)
            .await?
            .unwrap()
            .usage
            .input_tokens,
        3000
    );
    drop(agent);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn warming_errors_missing_usage_and_timeouts_still_send_the_real_request() -> Result<()> {
    for mode in [
        WarmResult::Failure,
        WarmResult::PartialFailure,
        WarmResult::NoUsage,
        WarmResult::Stall,
    ] {
        let (harness, session, request, calls, _) = fixture(mode).await?;
        let providers = harness
            .runtime
            .agent_manager
            .read()
            .await
            .cloned_provider_manager();
        let result = RuntimeCore::drive_stream(
            session.clone(),
            request,
            providers,
            harness.runtime.agent_manager.clone(),
            harness.runtime.event_tx.clone(),
            harness.runtime.session_state_barrier.clone(),
        )
        .await;
        assert!(matches!(result, StreamDriveResult::Completed(_)));
        assert_eq!(calls.lock().unwrap().len(), 2);
        let store = harness
            .runtime
            .agent_manager
            .read()
            .await
            .request_usage_store();
        let requests = store.load(&session).await?;
        assert_eq!(requests.len(), 2);
        assert!(requests.values().any(|request| {
            request.message_id.as_str().starts_with("cache-warming-")
                && request.usage.is_some() == matches!(mode, WarmResult::PartialFailure)
        }));
        harness.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn cancelling_warming_does_not_start_the_real_request_or_lose_accounting() -> Result<()> {
    let (harness, session, request, calls, entered) =
        fixture(WarmResult::WaitForCancellation).await?;
    let providers = harness
        .runtime
        .agent_manager
        .read()
        .await
        .cloned_provider_manager();
    let task = tokio::spawn(RuntimeCore::drive_stream(
        session.clone(),
        request,
        providers,
        harness.runtime.agent_manager.clone(),
        harness.runtime.event_tx.clone(),
        harness.runtime.session_state_barrier.clone(),
    ));
    entered.cancelled().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(calls.lock().unwrap().len(), 1);
    let store = harness
        .runtime
        .agent_manager
        .read()
        .await
        .request_usage_store();
    let requests = store.load(&session).await?;
    assert_eq!(requests.len(), 1);
    assert!(requests.values().all(|request| request.usage.is_none()));
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn invalid_warming_policy_does_not_block_the_real_request() -> Result<()> {
    let (harness, session, request, calls, _) = fixture(WarmResult::InvalidPolicy).await?;
    let providers = harness
        .runtime
        .agent_manager
        .read()
        .await
        .cloned_provider_manager();
    let result = RuntimeCore::drive_stream(
        session,
        request,
        providers,
        harness.runtime.agent_manager.clone(),
        harness.runtime.event_tx.clone(),
        harness.runtime.session_state_barrier.clone(),
    )
    .await;
    assert!(matches!(result, StreamDriveResult::Completed(_)));
    assert_eq!(calls.lock().unwrap().len(), 1);
    harness.shutdown().await;
    Ok(())
}
